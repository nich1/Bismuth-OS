//! The footprint snapshot: a plain, sorted report of what the system is made
//! of, compared with a stored copy so that any growth has to be explained.
//!
//! See "Before-and-after snapshots" in `docs/testing.md`.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use crate::Result;

/// Where the expected report is stored, relative to the repository root.
pub const STORED: &str = "snapshots/footprint.txt";

/// The folders whose Rust files are scanned for `unsafe`.
const SOURCES: &[&str] = &[
    "acpi/src",
    "apic/src",
    "boot/src",
    "descriptors/src",
    "elf/src",
    "frames/src",
    "handoff/src",
    "kernel/src",
    "paging/src",
];

/// How many `unsafe` items one Rust source file contains.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UnsafeCounts {
    /// `unsafe { ... }` blocks.
    pub blocks: usize,
    /// `unsafe fn name` and `unsafe extern "abi" fn name` declarations.
    pub fns: usize,
    /// `#[unsafe(...)]` attributes, such as `#[unsafe(no_mangle)]`.
    pub attributes: usize,
    /// `unsafe extern { ... }` and `unsafe extern "abi" { ... }` blocks.
    pub extern_blocks: usize,
    /// `unsafe impl` items.
    pub impls: usize,
}

/// Counts each kind of `unsafe` in Rust source.
///
/// `unsafe { ... }` is a block. `unsafe fn name` and
/// `unsafe extern "abi" fn name` are declarations. A function pointer type
/// such as `unsafe extern "efiapi" fn(...)` is neither. `unsafe(` is an
/// attribute, `unsafe extern {` and `unsafe extern "abi" {` are extern
/// blocks, and `unsafe impl` is an impl. `unsafe trait` is not counted.
/// Comments and literals are skipped. Fails on an unterminated comment or
/// literal, which would otherwise hide the rest of the file.
pub fn count_unsafe(source: &str) -> Result<UnsafeCounts> {
    let tokens = tokens(source)?;
    let mut counts = UnsafeCounts::default();
    for (i, token) in tokens.iter().enumerate() {
        if !is_word(token, "unsafe") {
            continue;
        }
        match &tokens[i + 1..] {
            [Token::Punct('{'), ..] => counts.blocks += 1,
            [fn_, Token::Ident(_), ..] if is_word(fn_, "fn") => counts.fns += 1,
            [extern_, Token::Literal, fn_, Token::Ident(_), ..]
            | [extern_, fn_, Token::Ident(_), ..]
                if is_word(extern_, "extern") && is_word(fn_, "fn") =>
            {
                counts.fns += 1
            }
            // Outside macro input, `unsafe (` is only ever an attribute, at
            // the top of one or inside `cfg_attr`.
            [Token::Punct('('), ..] => counts.attributes += 1,
            [extern_, Token::Literal, Token::Punct('{'), ..] | [extern_, Token::Punct('{'), ..]
                if is_word(extern_, "extern") =>
            {
                counts.extern_blocks += 1
            }
            [impl_, ..] if is_word(impl_, "impl") => counts.impls += 1,
            _ => {}
        }
    }
    Ok(counts)
}

/// The pieces of Rust source the counter looks at.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// A word: keyword, identifier or number. Raw identifiers keep `r#`.
    Ident(String),
    /// A single punctuation character.
    Punct(char),
    /// A string, byte-string, raw-string or character literal.
    Literal,
}

fn is_word(token: &Token, word: &str) -> bool {
    matches!(token, Token::Ident(ident) if ident == word)
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Splits Rust source into tokens, dropping whitespace and comments.
fn tokens(source: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = source.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = at(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && at(i + 1) == Some('/') {
            while at(i).is_some_and(|c| c != '\n') {
                i += 1;
            }
        } else if c == '/' && at(i + 1) == Some('*') {
            i = skip_block_comment(&chars, i)?;
        } else if c == '"' {
            i = skip_quoted(&chars, i, '"')?;
            out.push(Token::Literal);
        } else if c == '\'' {
            if at(i + 1).is_none() {
                return Err("unterminated literal".into());
            } else if at(i + 1) == Some('\\') || at(i + 2) == Some('\'') {
                i = skip_quoted(&chars, i, '\'')?;
                out.push(Token::Literal);
            } else {
                // A lifetime or loop label: drop it with its name, which in
                // macro input can be a keyword such as `'unsafe`, or a raw
                // name such as `'r#unsafe`. Since Rust 2021 a `#` straight
                // after a lifetime can only be that raw form.
                i += 1;
                while at(i).is_some_and(is_ident_char) {
                    i += 1;
                }
                if at(i) == Some('#') {
                    i += 1;
                    while at(i).is_some_and(is_ident_char) {
                        i += 1;
                    }
                }
            }
        } else if is_ident_char(c) {
            let start = i;
            while at(i).is_some_and(is_ident_char) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            // `b"..."`, `c"..."` and `b'...'` need no case here: the prefix is
            // read as a plain word and the quote branches above skip the rest.
            // Raw strings do, because their body has no escapes and may hold
            // a bare `"`.
            match (word.as_str(), at(i)) {
                ("r" | "br" | "cr", Some('"' | '#')) => {
                    let mut hashes = 0;
                    while at(i + hashes) == Some('#') {
                        hashes += 1;
                    }
                    if at(i + hashes) == Some('"') {
                        i = skip_raw_string(&chars, i + hashes, hashes)?;
                        out.push(Token::Literal);
                    } else {
                        // A raw identifier such as `r#unsafe`, which is not a
                        // keyword. Valid Rust has no other form here.
                        let name_start = i + hashes;
                        i = name_start;
                        while at(i).is_some_and(is_ident_char) {
                            i += 1;
                        }
                        let name: String = chars[name_start..i].iter().collect();
                        out.push(Token::Ident(format!("r#{name}")));
                    }
                }
                _ => out.push(Token::Ident(word)),
            }
        } else {
            out.push(Token::Punct(c));
            i += 1;
        }
    }
    Ok(out)
}

/// Skips a `/* ... */` comment starting at `start`, allowing nesting.
/// Returns the index just after it.
fn skip_block_comment(chars: &[char], start: usize) -> Result<usize> {
    let mut depth = 0;
    let mut i = start;
    while i < chars.len() {
        match (chars[i], chars.get(i + 1)) {
            ('/', Some('*')) => {
                depth += 1;
                i += 2;
            }
            ('*', Some('/')) => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => i += 1,
        }
    }
    Err("unterminated block comment".into())
}

/// Skips a literal that opens with `quote` at `start` and allows backslash
/// escapes. Returns the index just after the closing quote.
fn skip_quoted(chars: &[char], start: usize, quote: char) -> Result<usize> {
    let mut i = start + 1;
    loop {
        match chars.get(i) {
            None => return Err("unterminated literal".into()),
            Some('\\') => i += 2,
            Some(&c) if c == quote => return Ok(i + 1),
            Some(_) => i += 1,
        }
    }
}

/// Skips a raw string whose opening `"` is at `quote` and which closes with
/// `"` followed by `hashes` `#` characters. Returns the index after it.
fn skip_raw_string(chars: &[char], quote: usize, hashes: usize) -> Result<usize> {
    let mut i = quote + 1;
    while i < chars.len() {
        if chars[i] == '"' && (1..=hashes).all(|n| chars.get(i + n) == Some(&'#')) {
            return Ok(i + 1 + hashes);
        }
        i += 1;
    }
    Err("unterminated raw string".into())
}

/// Counts the third-party packages in a `Cargo.lock`: the `[[package]]`
/// entries that have a `source`. Packages in this repository have none.
pub fn third_party_packages(lock: &str) -> usize {
    let mut count = 0;
    let mut in_package = false;
    for line in lock.lines() {
        if line.starts_with('[') {
            in_package = line == "[[package]]";
        } else if in_package && line.starts_with("source =") {
            count += 1;
        }
    }
    count
}

/// Describes how `actual` differs from `expected`, or `None` if it does not.
///
/// Both are compared line by line, so `\r\n` and `\n` endings are the same.
/// Each removed line is shown as `- line` and each added one as `+ line`, in
/// sorted order, so a changed value shows its old line above its new one. A
/// line repeated more often on one side shows up once per extra copy.
pub fn difference(expected: &str, actual: &str) -> Option<String> {
    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    if expected == actual {
        return None;
    }
    let (mut old, mut new) = (expected.clone(), actual.clone());
    old.sort();
    new.sort();
    // Walk both sorted lists together; a line matched on both sides cancels.
    let (mut i, mut j) = (0, 0);
    let mut changes: Vec<(&str, char)> = Vec::new();
    while let (Some(&a), Some(&b)) = (old.get(i), new.get(j)) {
        match a.cmp(b) {
            Ordering::Equal => (i, j) = (i + 1, j + 1),
            Ordering::Less => {
                changes.push((a, '-'));
                i += 1;
            }
            Ordering::Greater => {
                changes.push((b, '+'));
                j += 1;
            }
        }
    }
    changes.extend(old[i..].iter().map(|&a| (a, '-')));
    changes.extend(new[j..].iter().map(|&b| (b, '+')));
    if changes.is_empty() {
        return Some("the same lines, but not in sorted order\n".into());
    }
    let key = |line: &str| {
        line.split_once(": ")
            .map_or(line, |(key, _)| key)
            .to_owned()
    };
    // The walk already lists each side in sorted order, and the sort is
    // stable, so equal keys keep that order.
    changes.sort_by_key(|&(line, sign)| (key(line), sign == '+'));
    Some(
        changes
            .iter()
            .map(|(line, sign)| format!("{sign} {line}\n"))
            .collect(),
    )
}

/// Everything the footprint report covers.
pub struct Footprint {
    /// Size of the built boot image file.
    pub boot_image_bytes: u64,
    /// Size of the kernel file the boot stage reads.
    pub kernel_file_bytes: u64,
    /// The pages the kernel's loadable segments take once placed.
    pub kernel_pages: u64,
    /// `unsafe` counts per source file, by forward-slash path.
    pub unsafe_by_file: Vec<(String, UnsafeCounts)>,
    /// Packages in `Cargo.lock` that come from outside the repository.
    pub third_party_packages: usize,
}

/// Formats the report as sorted `key: value` lines, each ending in `\n`.
pub fn report(footprint: &Footprint) -> String {
    let mut lines = vec![
        format!("boot image bytes: {}", footprint.boot_image_bytes),
        format!("kernel file bytes: {}", footprint.kernel_file_bytes),
        format!("kernel loaded pages: {}", footprint.kernel_pages),
        format!("third-party packages: {}", footprint.third_party_packages),
    ];
    for (path, counts) in &footprint.unsafe_by_file {
        lines.push(format!("unsafe blocks in {path}: {}", counts.blocks));
        lines.push(format!("unsafe fn declarations in {path}: {}", counts.fns));
        lines.push(format!(
            "unsafe attributes in {path}: {}",
            counts.attributes
        ));
        lines.push(format!(
            "unsafe extern blocks in {path}: {}",
            counts.extern_blocks
        ));
        lines.push(format!("unsafe impls in {path}: {}", counts.impls));
    }
    lines.sort();
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// Measures the footprint of the repository at `root`, whose boot image and
/// kernel file have already been built at `boot_image` and `kernel`.
pub fn measure(root: &Path, boot_image: &Path, kernel: &Path) -> Result<Footprint> {
    let boot_image_bytes = fs::metadata(boot_image)
        .map_err(|error| format!("could not read {}: {error}", boot_image.display()))?
        .len();

    let mut files = Vec::new();
    // In the order the folder lists them; [`report`] sorts its lines.
    for dir in SOURCES {
        rust_files(&root.join(dir), dir, &mut files)?;
    }
    let mut unsafe_by_file = Vec::new();
    for path in files {
        let source = fs::read_to_string(root.join(&path))
            .map_err(|error| format!("could not read {path}: {error}"))?;
        let counts = count_unsafe(&source).map_err(|error| format!("{path}: {error}"))?;
        unsafe_by_file.push((path, counts));
    }

    let lock = fs::read_to_string(root.join("Cargo.lock"))
        .map_err(|error| format!("could not read Cargo.lock: {error}"))?;

    let kernel = fs::read(kernel)
        .map_err(|error| format!("could not read {}: {error}", kernel.display()))?;
    let elf = bismuth_elf::Elf::parse(&kernel)
        .map_err(|error| format!("the ELF reader rejected the kernel: {error:?}"))?;
    let placement = bismuth_elf::place::place(&elf)
        .map_err(|error| format!("the kernel cannot be placed as linked: {error:?}"))?;
    Ok(Footprint {
        boot_image_bytes,
        kernel_file_bytes: kernel.len() as u64,
        kernel_pages: placement.pages(),
        unsafe_by_file,
        third_party_packages: third_party_packages(&lock),
    })
}

/// Collects the `.rs` files under `dir`, named `prefix/...` with forward
/// slashes so the report reads the same on every host.
fn rust_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|error| format!("could not list {prefix}: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not list {prefix}: {error}"))?;
        let path = entry_path(prefix, &entry.file_name())?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect {path}: {error}"))?;
        if kind.is_dir() {
            rust_files(&entry.path(), &path, out)?;
        } else if path.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Names the folder entry `name` inside `prefix` as report text, joined with
/// a forward slash on every host. Fails if the name is not UTF-8, which the
/// report could not show faithfully.
fn entry_path(prefix: &str, name: &OsStr) -> Result<String> {
    match name.to_str() {
        Some(name) => Ok(format!("{prefix}/{name}")),
        None => Err(format!("{prefix} holds a file whose name is not UTF-8")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bismuth_elf::place::KERNEL_BASE;

    fn counts(source: &str) -> (usize, usize) {
        let counts = count_unsafe(source).expect("source scans");
        (counts.blocks, counts.fns)
    }

    #[test]
    fn counts_blocks_and_declarations() {
        let source = "unsafe fn a() { unsafe { b() } }\n\
                      pub unsafe extern \"efiapi\" fn c() {}\n\
                      const unsafe fn d() {}\n\
                      let x = unsafe { *p };";
        assert_eq!(counts(source), (2, 3));
    }

    #[test]
    fn a_function_pointer_type_is_not_a_declaration() {
        let source = "struct S {\n    f: unsafe extern \"efiapi\" fn(x: u8) -> u8,\n    \
                      g: unsafe fn(),\n    h: unsafe extern fn(),\n}";
        assert_eq!(counts(source), (0, 0));
    }

    #[test]
    fn only_extern_and_fn_make_a_declaration() {
        // A macro accepts any tokens, so these are valid source.
        let source = "m!(unsafe safe fn a); m!(unsafe extern crate b);\n\
                      m!(unsafe extern safe fn c);";
        assert_eq!(counts(source), (0, 0));
    }

    #[test]
    fn a_word_containing_unsafe_is_not_the_keyword() {
        assert_eq!(counts("let s = not_unsafe { a: 1 };"), (0, 0));
    }

    #[test]
    fn division_is_not_a_comment() {
        assert_eq!(
            counts("let a = b / c; unsafe { d }\nlet e = f /g; unsafe { h }"),
            (2, 0)
        );
    }

    #[test]
    fn raw_strings_ending_in_a_backslash() {
        let source = r#"
            let a = r"a\"; unsafe { x }
            let b = br"b\"; unsafe { y }
            let c = cr"c\"; unsafe { z }
        "#;
        assert_eq!(counts(source), (3, 0));
    }

    #[test]
    fn impls_extern_blocks_and_attributes_are_not_blocks_or_declarations() {
        let source = "unsafe impl Send for S {}\n\
                      unsafe extern \"C\" { fn f(); }\n\
                      #[unsafe(no_mangle)]\nfn g() {}";
        assert_eq!(counts(source), (0, 0));
        assert_eq!(other_kinds(source), (1, 1, 1));
    }

    /// The `#[unsafe(...)]` attributes, `unsafe extern` blocks and
    /// `unsafe impl`s in `source`.
    fn other_kinds(source: &str) -> (usize, usize, usize) {
        let counts = count_unsafe(source).expect("source scans");
        (counts.attributes, counts.extern_blocks, counts.impls)
    }

    #[test]
    fn counts_unsafe_attributes() {
        let source = "#[unsafe(no_mangle)]\n#[unsafe(naked)]\nfn a() {}\n\
                      #![unsafe(no_builtins)]\n\
                      #[cfg_attr(test, unsafe(export_name = \"b\"))]\nfn b() {}";
        assert_eq!(other_kinds(source), (4, 0, 0));
    }

    #[test]
    fn an_attribute_in_a_comment_or_a_string_is_not_counted() {
        let source = r####"
            // #[unsafe(no_mangle)]
            /// #[unsafe(no_mangle)]
            /* #[unsafe(naked)] /* #[unsafe(naked)] */ */
            let a = "#[unsafe(no_mangle)]";
            let b = r#"#[unsafe(no_mangle)] " unsafe(x)"#;
            m!('unsafe (x));
            let c = r#unsafe(1);
        "####;
        assert_eq!(other_kinds(source), (0, 0, 0));
    }

    #[test]
    fn only_a_parenthesis_makes_an_attribute() {
        // A macro accepts any tokens, so these are valid source.
        let source = "m!(unsafe [x]); m!(unsafe \"x\"); m!(unsafe ! x); m!(unsafe < x);";
        assert_eq!(other_kinds(source), (0, 0, 0));
    }

    #[test]
    fn counts_unsafe_extern_blocks() {
        let source = "unsafe extern \"C\" { fn f(); }\n\
                      pub unsafe extern { safe fn g(); }\n\
                      extern \"C\" { fn h(); }";
        assert_eq!(other_kinds(source), (0, 2, 0));
    }

    #[test]
    fn an_extern_function_or_type_is_not_an_extern_block() {
        let source = "pub unsafe extern \"efiapi\" fn a() {}\n\
                      type B = unsafe extern \"C\" fn();\n\
                      // unsafe extern \"C\" {\n\
                      let c = \"unsafe extern {\";\n\
                      m!(unsafe extern crate d {});";
        assert_eq!(other_kinds(source), (0, 0, 0));
    }

    #[test]
    fn counts_unsafe_impls() {
        let source = "unsafe impl Send for S {}\n\
                      unsafe impl<T> Sync for H<T> {}\n\
                      impl Drop for S { fn drop(&mut self) {} }";
        assert_eq!(other_kinds(source), (0, 0, 2));
    }

    #[test]
    fn an_impl_in_a_comment_or_a_string_is_not_counted() {
        let source = "// unsafe impl Send for S {}\n\
                      /* unsafe impl Sync for S {} */\n\
                      let a = \"unsafe impl\";\n\
                      m!('unsafe impl); m!(r#unsafe impl); m!(not_unsafe impl);";
        assert_eq!(other_kinds(source), (0, 0, 0));
    }

    #[test]
    fn ignores_comments() {
        let source = "// unsafe { a }\n/// unsafe fn b()\n\
                      /* outer /* unsafe { c } */ unsafe { d } */\n\
                      unsafe { e } /* f */ unsafe { g }";
        assert_eq!(counts(source), (2, 0));
    }

    #[test]
    fn ignores_string_and_character_literals() {
        let source = r####"
            let a = "unsafe { x } \" unsafe { y }";
            let b = r#"unsafe { " unsafe fn z() }"#;
            let c = b"unsafe {";
            let d = br##"unsafe { "# }"##;
            let e = '"'; unsafe { f }
            let g = b'"'; let h = '\''; let i = '{'; let q = '\"'; unsafe { r }
            fn j<'a>(k: &'a str) -> &'a str { unsafe { k } }
        "####;
        assert_eq!(counts(source), (3, 0));
    }

    #[test]
    fn counts_an_extern_declaration_without_an_abi() {
        assert_eq!(counts("unsafe extern fn a() {}"), (0, 1));
    }

    #[test]
    fn ignores_c_string_literals() {
        let source = r####"
            let a = c"unsafe { x }";
            let b = cr"unsafe { y }";
            let c = cr#"unsafe { " unsafe { z }"#;
        "####;
        assert_eq!(counts(source), (0, 0));
    }

    #[test]
    fn rejects_a_lone_quote_at_the_end() {
        assert!(count_unsafe("let c = '").is_err());
    }

    #[test]
    fn a_lifetime_named_like_a_keyword_is_not_the_keyword() {
        // A macro accepts these tokens, so they are valid source.
        assert_eq!(counts("m!('unsafe { x });"), (0, 0));
        assert_eq!(counts("m!('unsafe fn x);"), (0, 0));
        assert_eq!(counts("m!('r#unsafe { x });"), (0, 0));
        // A real keyword after a lifetime still counts.
        assert_eq!(counts("m!('r unsafe { x });"), (1, 0));
        assert_eq!(
            counts("'outer: loop { break 'outer; } unsafe { y }"),
            (1, 0)
        );
        assert_eq!(counts("let s: &'static str = \"\"; unsafe { z }"), (1, 0));
    }

    #[test]
    fn a_raw_identifier_is_not_the_keyword() {
        assert_eq!(counts("let r#unsafe = 1; r#unsafe {}"), (0, 0));
    }

    #[test]
    fn rejects_unterminated_comments_and_literals() {
        assert!(count_unsafe("/* unsafe { a }").is_err());
        assert!(count_unsafe("let s = \"unsafe { a }").is_err());
        assert!(count_unsafe("let s = r#\"unsafe { a }\"").is_err());
    }

    #[test]
    fn counts_only_packages_with_a_source() {
        let lock = "version = 4\n\n\
                    [[package]]\nname = \"bismuth-boot\"\nversion = \"0.0.0\"\n\n\
                    [[package]]\nname = \"outside\"\nversion = \"1.0.0\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n\
                    [[package]]\nname = \"xtask\"\nversion = \"0.0.0\"\n";
        assert_eq!(third_party_packages(lock), 1);
    }

    #[test]
    fn a_lock_file_with_only_local_packages_has_none() {
        let lock = "version = 4\n\n[[package]]\nname = \"xtask\"\nversion = \"0.0.0\"\n";
        assert_eq!(third_party_packages(lock), 0);
    }

    #[test]
    fn unused_patches_are_not_packages() {
        let lock = "version = 4\n\n\
                    [[package]]\nname = \"xtask\"\nversion = \"0.0.0\"\n\n\
                    [[patch.unused]]\nname = \"outside\"\nversion = \"1.0.0\"\n\
                    source = \"git+https://example.com/outside\"\n";
        assert_eq!(third_party_packages(lock), 0);
    }

    #[test]
    fn matching_reports_have_no_difference() {
        assert_eq!(difference("a: 1\nb: 2\n", "a: 1\nb: 2\n"), None);
    }

    #[test]
    fn line_endings_make_no_difference() {
        assert_eq!(difference("a: 1\r\nb: 2\r\n", "a: 1\nb: 2\n"), None);
    }

    #[test]
    fn shows_removed_and_added_lines() {
        let text = difference("a: 1\nb: 2\nc: 3\n", "a: 1\nb: 3\nd: 4\n").expect("differs");
        assert_eq!(text, "- b: 2\n+ b: 3\n- c: 3\n+ d: 4\n");
    }

    #[test]
    fn a_matching_line_after_a_mismatch_is_not_shown() {
        let text = difference("a: 1\nc: 3\n", "b: 2\nc: 3\n").expect("differs");
        assert_eq!(text, "- a: 1\n+ b: 2\n");
    }

    #[test]
    fn a_missing_repeat_of_a_line_is_shown() {
        let text = difference("a: 1\na: 1\nb: 2\n", "a: 1\nb: 3\n").expect("differs");
        assert_eq!(text, "- a: 1\n- b: 2\n+ b: 3\n");
    }

    /// A folder under the system temporary folder, removed when dropped.
    struct TempTree(std::path::PathBuf);

    impl TempTree {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("bismuth-footprint-{name}-{}", std::process::id()));
            // A leftover from an earlier, interrupted run would skew the test.
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temporary folder is created");
            TempTree(path)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("has a folder")).expect("folder");
            fs::write(path, contents).expect("file is written");
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn measures_nested_files_with_forward_slash_names() {
        let tree = TempTree::new("measure");
        tree.write("acpi/src/lib.rs", "fn l() { unsafe { m() } }");
        tree.write("apic/src/lib.rs", "unsafe fn n() { unsafe { o() } }");
        tree.write(
            "boot/src/main.rs",
            "#[unsafe(no_mangle)]
unsafe fn a() {}",
        );
        tree.write("boot/src/notes.txt", "unsafe { not rust }");
        tree.write("boot/src/inner/deep.rs", "fn b() { unsafe { c() } }");
        tree.write("descriptors/src/lib.rs", "fn j() { unsafe { k() } }");
        tree.write("elf/src/lib.rs", "fn d() {}");
        tree.write("frames/src/lib.rs", "unsafe fn i() {}");
        tree.write("handoff/src/lib.rs", "unsafe fn i() {}");
        tree.write(
            "kernel/src/main.rs",
            "unsafe fn e() { unsafe { f() } }
unsafe impl Sync for S {}
unsafe extern {}",
        );
        tree.write("paging/src/lib.rs", "fn g() { unsafe { h() } }");
        tree.write(
            "Cargo.lock",
            "[[package]]\nname = \"outside\"\nsource = \"registry+https://example.com\"\n",
        );
        tree.write("image.efi", "12345");
        // Two pages: one from the file, one more of memory past it.
        let mut kernel = crate::tests::kernel_file(0b101, KERNEL_BASE + 0x40, 1);
        kernel[104..112].copy_from_slice(&0x2000u64.to_le_bytes());
        fs::write(tree.0.join("kernel.elf"), &kernel).expect("file is written");

        let mut footprint = measure(
            &tree.0,
            &tree.0.join("image.efi"),
            &tree.0.join("kernel.elf"),
        )
        .expect("measures");
        assert_eq!(footprint.boot_image_bytes, 5);
        assert_eq!(footprint.kernel_file_bytes, 4096);
        assert_eq!(footprint.kernel_pages, 2);
        assert_eq!(footprint.third_party_packages, 1);
        // The folder decides the order; the report sorts it later.
        footprint.unsafe_by_file.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            footprint.unsafe_by_file,
            [
                (
                    "acpi/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 1,
                        fns: 0,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "apic/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 1,
                        fns: 1,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "boot/src/inner/deep.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 1,
                        fns: 0,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "boot/src/main.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 0,
                        fns: 1,
                        attributes: 1,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "descriptors/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 1,
                        fns: 0,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "elf/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 0,
                        fns: 0,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "frames/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 0,
                        fns: 1,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "handoff/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 0,
                        fns: 1,
                        ..UnsafeCounts::default()
                    }
                ),
                (
                    "kernel/src/main.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 1,
                        fns: 1,
                        attributes: 0,
                        extern_blocks: 1,
                        impls: 1,
                    }
                ),
                (
                    "paging/src/lib.rs".to_owned(),
                    UnsafeCounts {
                        blocks: 1,
                        fns: 0,
                        ..UnsafeCounts::default()
                    }
                ),
            ]
        );
    }

    #[test]
    fn measuring_fails_without_boot_sources() {
        let tree = TempTree::new("missing");
        tree.write("acpi/src/lib.rs", "fn a() {}");
        tree.write("apic/src/lib.rs", "fn a() {}");
        tree.write("image.efi", "1");
        let error = measure(
            &tree.0,
            &tree.0.join("image.efi"),
            &tree.0.join("kernel.elf"),
        )
        .err()
        .expect("fails");
        assert!(error.contains("could not list boot/src"), "{error}");
    }

    #[test]
    fn measuring_fails_without_kernel_sources() {
        let tree = TempTree::new("no-kernel");
        tree.write("acpi/src/lib.rs", "fn a() {}");
        tree.write("apic/src/lib.rs", "fn a() {}");
        tree.write("boot/src/main.rs", "fn a() {}");
        tree.write("descriptors/src/lib.rs", "fn d() {}");
        tree.write("elf/src/lib.rs", "fn b() {}");
        tree.write("frames/src/lib.rs", "fn c() {}");
        tree.write("handoff/src/lib.rs", "fn c() {}");
        tree.write("image.efi", "1");
        let error = measure(
            &tree.0,
            &tree.0.join("image.efi"),
            &tree.0.join("kernel.elf"),
        )
        .err()
        .expect("fails");
        assert!(error.contains("could not list kernel/src"), "{error}");
    }

    #[test]
    fn measuring_names_what_failed() {
        let tree = TempTree::new("errors");
        tree.write("acpi/src/lib.rs", "fn a() {}");
        tree.write("apic/src/lib.rs", "fn a() {}");
        tree.write("boot/src/main.rs", "fn a() {}");
        tree.write("descriptors/src/lib.rs", "fn f() {}");
        tree.write("elf/src/lib.rs", "fn b() {}");
        tree.write("frames/src/lib.rs", "fn e() {}");
        tree.write("handoff/src/lib.rs", "fn e() {}");
        tree.write("kernel/src/main.rs", "fn c() {}");
        tree.write("paging/src/lib.rs", "fn d() {}");
        let image = tree.0.join("image.efi");
        let kernel = tree.0.join("kernel.elf");

        let error = measure(&tree.0, &image, &kernel).err().expect("no image");
        assert!(error.starts_with("could not read ") && error.contains("image.efi"));

        tree.write("image.efi", "1");
        let error = measure(&tree.0, &image, &kernel)
            .err()
            .expect("no lock file");
        assert!(error.starts_with("could not read Cargo.lock"), "{error}");

        tree.write("Cargo.lock", "version = 4\n");
        tree.write("boot/src/bad.rs", "/* never closed");
        let error = measure(&tree.0, &image, &kernel).err().expect("bad source");
        assert_eq!(error, "boot/src/bad.rs: unterminated block comment");

        fs::remove_file(tree.0.join("boot/src/bad.rs")).expect("file is removed");
        let error = measure(&tree.0, &image, &kernel).err().expect("no kernel");
        assert!(
            error.starts_with("could not read ") && error.contains("kernel.elf"),
            "{error}"
        );

        tree.write("kernel.elf", "not an ELF file");
        let error = measure(&tree.0, &image, &kernel).err().expect("bad kernel");
        assert_eq!(error, "the ELF reader rejected the kernel: Truncated");
    }

    #[test]
    fn rejects_a_file_name_that_is_not_utf8() {
        // Built in memory: some file systems refuse to store such a name.
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[0xD800])
        };
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![0xFF])
        };
        assert_eq!(
            entry_path("boot/src", &name),
            Err("boot/src holds a file whose name is not UTF-8".to_owned())
        );
    }

    #[test]
    fn an_entry_path_uses_a_forward_slash() {
        let name = std::ffi::OsStr::new("main.rs");
        assert_eq!(
            entry_path("boot/src", name),
            Ok("boot/src/main.rs".to_owned())
        );
    }

    #[test]
    fn a_file_in_the_wrong_order_is_reported() {
        let message = Some("the same lines, but not in sorted order\n");
        assert_eq!(
            difference("b: 2\na: 1\n", "a: 1\nb: 2\n").as_deref(),
            message
        );
        assert_eq!(
            difference("a: 1\nb: 2\n", "b: 2\na: 1\n").as_deref(),
            message
        );
    }

    #[test]
    fn a_raw_string_ends_after_its_closing_hashes() {
        let chars: Vec<char> = r###"r##"a"#b"##c"###.chars().collect();
        // The opening quote is at index 3; the text after the string is `c`.
        assert_eq!(skip_raw_string(&chars, 3, 2), Ok(11));
    }

    #[test]
    fn measuring_rejects_a_source_file_that_is_not_utf8() {
        let tree = TempTree::new("bytes");
        tree.write("Cargo.lock", "version = 4\n");
        tree.write("image.efi", "1");
        tree.write("acpi/src/lib.rs", "");
        tree.write("apic/src/lib.rs", "");
        tree.write("boot/src/main.rs", "");
        tree.write("descriptors/src/lib.rs", "");
        tree.write("elf/src/lib.rs", "");
        tree.write("frames/src/lib.rs", "");
        tree.write("handoff/src/lib.rs", "");
        tree.write("kernel/src/main.rs", "");
        tree.write("paging/src/lib.rs", "");
        fs::write(tree.0.join("boot/src/main.rs"), [0xFF]).expect("file is written");
        let error = measure(
            &tree.0,
            &tree.0.join("image.efi"),
            &tree.0.join("kernel.elf"),
        )
        .err()
        .expect("fails");
        assert!(
            error.starts_with("could not read boot/src/main.rs: "),
            "{error}"
        );
    }

    #[test]
    fn a_shrinking_value_shows_the_old_line_first() {
        let text = difference("a: 3\n", "a: 2\n").expect("differs");
        assert_eq!(text, "- a: 3\n+ a: 2\n");
    }

    #[test]
    fn report_is_sorted_with_forward_slash_paths() {
        let footprint = Footprint {
            boot_image_bytes: 4096,
            kernel_file_bytes: 53760,
            kernel_pages: 29,
            unsafe_by_file: vec![
                (
                    "boot/src/main.rs".into(),
                    UnsafeCounts {
                        blocks: 2,
                        fns: 1,
                        attributes: 5,
                        extern_blocks: 6,
                        impls: 7,
                    },
                ),
                (
                    "boot/src/a.rs".into(),
                    UnsafeCounts {
                        blocks: 4,
                        fns: 3,
                        ..UnsafeCounts::default()
                    },
                ),
            ],
            third_party_packages: 7,
        };
        assert_eq!(
            report(&footprint),
            "boot image bytes: 4096\n\
             kernel file bytes: 53760\n\
             kernel loaded pages: 29\n\
             third-party packages: 7\n\
             unsafe attributes in boot/src/a.rs: 0\n\
             unsafe attributes in boot/src/main.rs: 5\n\
             unsafe blocks in boot/src/a.rs: 4\n\
             unsafe blocks in boot/src/main.rs: 2\n\
             unsafe extern blocks in boot/src/a.rs: 0\n\
             unsafe extern blocks in boot/src/main.rs: 6\n\
             unsafe fn declarations in boot/src/a.rs: 3\n\
             unsafe fn declarations in boot/src/main.rs: 1\n\
             unsafe impls in boot/src/a.rs: 0\n\
             unsafe impls in boot/src/main.rs: 7\n"
        );
    }
}
