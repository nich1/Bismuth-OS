//! Build, run and test tasks for Bismuth.
//!
//! Run with `cargo xtask <task>`. This is an ordinary program for the
//! development machine, so the same commands work on Windows, Linux and macOS
//! without shell scripts.

use std::env;
use std::fs;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bismuth_elf::place;

#[cfg(test)]
mod command_tables;
mod footprint;
mod host;

// The boot stage's report text has no firmware code, so its unit tests run
// here, on the development machine, where the boot stage itself cannot.
#[cfg(test)]
#[path = "../../boot/src/report.rs"]
mod boot_report;
// The same for the order of the calls that end boot services. Named as in
// the boot stage, where the report text finds it as `crate::leave`.
#[cfg(test)]
#[path = "../../boot/src/leave.rs"]
mod leave;
// The same for building the hand-off structure from the memory map.
#[cfg(test)]
#[path = "../../boot/src/handoff.rs"]
mod handoff;
// The same for the kernel's checks of the state it was entered in.
#[cfg(test)]
#[path = "../../kernel/src/entry.rs"]
mod kernel_entry;
// And for its fault report and the frame its entry stubs build.
#[cfg(test)]
#[path = "../../kernel/src/fault.rs"]
mod kernel_fault;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  build   build the boot image and the kernel into target/esp
  run     build, then boot it in QEMU with a window and serial output
  test    build, then boot each boot scenario in QEMU without a window and
          check what it printed and how QEMU exited
  snapshot [--update]
          build, then print the footprint report; --update also stores it
          in snapshots/footprint.txt
  check   what the CI workflow runs on a pull request: feature map check,
          format, lint, unit tests, footprint snapshot, kernel file checks,
          reproducible kernel file, boot scenarios (not the text checks)

code host tasks (need gh, GitHub's command-line tool):
  issue-list [--label <name>]
          print the number and title of each open issue, one per line
  issue-read <number>
  issue-comment <number> --body-file <path>
  issue-new --title <text> --body-file <path> [--label <name>]
  pr-open --title <text> --body-file <path> [--draft]
          push the current branch and open a pull request against main
  pr-edit --body-file <path>
  pr-ready
  pr-wait [<number>]
          wait for every check on the pull request to finish";

/// The Rust target for programs the UEFI firmware can load.
const UEFI_TARGET: &str = "x86_64-unknown-uefi";
/// The package that becomes the boot image.
const BOOT_PACKAGE: &str = "bismuth-boot";
/// The Rust target for the kernel, which produces an ELF file.
const KERNEL_TARGET: &str = "x86_64-unknown-none";
/// The package that becomes the kernel.
const KERNEL_PACKAGE: &str = "bismuth-kernel";
/// The text the boot stage must print on the serial port.
const BOOT_MARKER: &str = "BISMUTH BOOT OK";
/// What QEMU exited with when the boot stage reported success, before the
/// kernel ran: `(0x10 << 1) | 1`. Nothing exits with it now, since the boot
/// stage jumps to the kernel instead; the tests check no scenario expects it.
#[cfg(test)]
const QEMU_SUCCESS_EXIT: i32 = 33;
/// What QEMU exits with when the boot stage reports that the kernel file is
/// missing or wrong: `(0x11 << 1) | 1`.
const QEMU_FAILURE_EXIT: i32 = 35;
/// What QEMU exits with when the kernel has started and printed its line:
/// `(0x20 << 1) | 1`.
const KERNEL_SUCCESS_EXIT: i32 = 65;
/// What QEMU exits with when the kernel panics: `(0x21 << 1) | 1`.
const KERNEL_FAILURE_EXIT: i32 = 67;
/// The line the kernel prints once it runs.
const KERNEL_MARKER: &str = "BISMUTH KERNEL OK";
/// The line the kernel's panic handler prints.
const KERNEL_PANIC: &str = "BISMUTH KERNEL PANIC";
/// The line the kernel prints once it has loaded its own tables.
const TABLES_MARKER: &str = "BISMUTH KERNEL TABLES OK";
/// How long one boot scenario may take before it is treated as a hang.
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);
/// Where the boot scenarios' disks are laid out, one folder each.
const SCENARIO_DISKS: &str = "target/boot-scenarios";

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["build"] => build(false),
        ["run"] => run(),
        ["test"] => test(),
        ["snapshot"] => snapshot(false),
        ["snapshot", "--update"] => snapshot(true),
        ["check"] => check(),
        ["issue-list"] => host::issue_list(None),
        ["issue-list", "--label", label] => host::issue_list(Some(label)),
        ["issue-read", n] if is_number(n) => host::issue_read(n),
        ["issue-comment", n, "--body-file", body] if is_number(n) => host::issue_comment(n, body),
        ["issue-new", "--title", title, "--body-file", body] => host::issue_new(title, body, None),
        [
            "issue-new",
            "--title",
            title,
            "--body-file",
            body,
            "--label",
            label,
        ] => host::issue_new(title, body, Some(label)),
        ["pr-open", "--title", title, "--body-file", body] => host::pr_open(title, body, false),
        ["pr-open", "--title", title, "--body-file", body, "--draft"] => {
            host::pr_open(title, body, true)
        }
        ["pr-edit", "--body-file", body] => host::pr_edit(body),
        ["pr-ready"] => host::pr_ready(),
        ["pr-wait"] => host::pr_wait(None),
        ["pr-wait", n] if is_number(n) => host::pr_wait(Some(n)),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// True for an issue or pull request number.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// The repository root: the parent of this package's directory.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repository root")
        .to_path_buf()
}

/// A `cargo` command that runs from the repository root.
fn cargo() -> Command {
    let mut command = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    command.current_dir(root());
    command
}

/// Runs a command and fails if it does not exit successfully.
fn run_checked(command: &mut Command, what: &str) -> Result<()> {
    let status = command
        .status()
        .map_err(|error| format!("could not start {what}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

/// Builds the boot stage and the kernel and lays them out as a bootable disk
/// in `target/esp`.
///
/// UEFI firmware boots a disk by running `EFI/BOOT/BOOTX64.EFI` from it, so
/// that is where the boot stage is copied. The kernel goes to
/// `EFI/BISMUTH/KERNEL.ELF`. With `exit_when_done`, the boot stage and the
/// kernel are both built with their `qemu-exit` features, to make
/// QEMU quit with their own exit codes: the boot stage on a failure, the
/// kernel once it runs.
fn build(exit_when_done: bool) -> Result<()> {
    let features: &[&str] = if exit_when_done { &["qemu-exit"] } else { &[] };
    copy_file(&build_boot(features)?, &boot_image())?;
    copy_file(&build_kernel(features)?, &kernel_image())
}

/// Builds the boot stage with `features` and returns where the file is.
/// Each build replaces the one before it at that place.
fn build_boot(features: &[&str]) -> Result<PathBuf> {
    let mut command = cargo();
    command.args(["build", "--package", BOOT_PACKAGE, "--target", UEFI_TARGET]);
    if !features.is_empty() {
        command.args(["--features", &features.join(",")]);
    }
    run_checked(&mut command, "cargo build")?;
    Ok(root()
        .join("target")
        .join(UEFI_TARGET)
        .join("debug")
        .join(format!("{BOOT_PACKAGE}.efi")))
}

/// Builds the kernel with `features` and returns where its lean file is (see
/// [`build_kernel_in`]).
fn build_kernel(features: &[&str]) -> Result<PathBuf> {
    build_kernel_in(&root(), features)
}

/// Builds the kernel of the repository at `root` with `features`, writes
/// its full and lean files (see [`split_kernel`]) next to the linker's
/// output, and returns where the lean file is. Each build replaces the one
/// before it at that place.
///
/// The kernel is built with [`remap_config`], so the folder it is built in
/// leaves no trace in the file.
fn build_kernel_in(root: &Path, features: &[&str]) -> Result<PathBuf> {
    let mut command = cargo();
    command.current_dir(root).args([
        "build",
        "--package",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--config",
        &remap_config(root)?,
    ]);
    if !features.is_empty() {
        command.args(["--features", &features.join(",")]);
    }
    run_checked(&mut command, "cargo build (kernel)")?;
    let linked = linked_kernel(root);
    let bytes = fs::read(&linked)
        .map_err(|error| format!("could not read {}: {error}", linked.display()))?;
    let (full, lean) = split_kernel(&bytes)?;
    for (path, bytes) in [(full_kernel(root), full), (lean_kernel(root), lean)] {
        fs::write(&path, bytes)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    Ok(lean_kernel(root))
}

/// Where cargo puts the kernel file the linker made, for the repository at
/// `root`.
fn linked_kernel(root: &Path) -> PathBuf {
    root.join("target")
        .join(KERNEL_TARGET)
        .join("debug")
        .join(KERNEL_PACKAGE)
}

/// Where [`build_kernel_in`] puts the full kernel file, kept for debugging.
fn full_kernel(root: &Path) -> PathBuf {
    linked_kernel(root).with_file_name(format!("{KERNEL_PACKAGE}.full"))
}

/// Where [`build_kernel_in`] puts the lean kernel file, the one that goes on
/// the boot disk.
fn lean_kernel(root: &Path) -> PathBuf {
    linked_kernel(root).with_file_name(format!("{KERNEL_PACKAGE}.lean"))
}

/// A `--config` value that adds `--remap-path-prefix=<root>=.` to the
/// kernel target's flags. Cargo joins it to the flags in
/// `.cargo/config.toml`, which cannot name the folder themselves. Without
/// it the debug information holds the folder the kernel was built in (as
/// the compilation directory), so the file's bytes and size change with
/// the folder.
fn remap_config(root: &Path) -> Result<String> {
    let root = root
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", root.display()))?;
    let flag = format!("--remap-path-prefix={root}=.");
    // A TOML basic string: backslash and quote escaped, and no control
    // characters, which a folder name has no use for.
    if flag.chars().any(char::is_control) {
        return Err(format!("{root} holds a control character"));
    }
    let quoted = flag.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("target.{KERNEL_TARGET}.rustflags=[\"{quoted}\"]"))
}

/// The kernel file the linker made, `linked`, as two files: a full one with
/// every section, for debugging, and a lean one with only what a loader
/// reads. Returns `(full, lean)`.
///
/// The linker puts the file header in the first loadable segment, so the
/// offset of the section header table is loaded with the kernel. Both files
/// therefore keep that table at one offset, just after the loaded contents
/// (the file header, the program header table and every segment's bytes),
/// and are the same up to it: the full file moves every section that lies
/// after the loaded contents to after the table, the section names first;
/// the lean file keeps only the names, and marks the other moved sections
/// as taking no space in the file (gABI ch. 4, "Sections", `SHT_NOBITS`).
fn split_kernel(linked: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    bismuth_elf::Elf::parse(linked)
        .map_err(|error| format!("the ELF reader rejected the kernel: {error:?}"))?;
    // gABI ch. 5, "Program Header": `p_offset` is the eight bytes at 8 and
    // `p_filesz` the eight at 32. The reader checked the table is inside.
    let programs = get_u64(linked, 32) as usize;
    let count = usize::from(u16::from_le_bytes([linked[56], linked[57]]));
    let mut loaded = (programs + 56 * count).max(64);
    for index in 0..count {
        let at = programs + 56 * index;
        match end_in(linked, get_u64(linked, at + 8), get_u64(linked, at + 32)) {
            Some(end) => loaded = loaded.max(end),
            None => return Err(format!("program header {index} reaches past the file")),
        }
    }
    // gABI ch. 4, "ELF Header": `e_shoff` is the eight bytes at 40,
    // `e_shentsize`, `e_shnum` and `e_shstrndx` the two each at 58, 60, 62.
    let field = |at: usize| usize::from(u16::from_le_bytes([linked[at], linked[at + 1]]));
    let (sections, names) = (field(60), field(62));
    let old_table = end_in(linked, get_u64(linked, 40), 64 * sections as u64)
        .filter(|_| field(58) == 64 && names < sections)
        .ok_or("the kernel's section header table is not as the linker writes it")?
        - 64 * sections;
    let table = loaded.next_multiple_of(8);
    let mut full = linked[..loaded].to_vec();
    full.resize(table, 0);
    full.extend_from_slice(&linked[old_table..old_table + 64 * sections]);
    set_u64(&mut full, 40, table as u64);
    // gABI ch. 4, "Sections": `sh_type` is the four bytes at 4, `sh_offset`
    // the eight at 24, `sh_size` at 32 and `sh_addralign` at 48.
    let mut moved = Vec::new();
    // The moved sections lie after the loaded contents and, as the linker
    // writes them, share no bytes, so together they take at most what
    // follows. A file whose sections take more would make the full file
    // that many times larger than itself.
    let (room, mut taken) = (linked.len() - loaded, 0);
    for index in std::iter::once(names).chain((0..sections).filter(|&i| i != names)) {
        let header = table + 64 * index;
        let (offset, size) = (get_u64(&full, header + 24), get_u64(&full, header + 32));
        let end = end_in(linked, offset, size);
        if full[header + 4..header + 8] == SHT_NOBITS || end.is_some_and(|end| end <= loaded) {
            continue;
        }
        let Some(end) = end.filter(|_| offset as usize >= loaded) else {
            return Err(format!(
                "section {index} is partly loaded or outside the file"
            ));
        };
        // `end_in` placed the section inside the file, so `offset` fits.
        let start = offset as usize;
        taken += end - start;
        if taken > room {
            return Err(format!(
                "section {index} and the sections moved before it take more bytes than follow the loaded contents"
            ));
        }
        let at = full
            .len()
            .next_multiple_of(section_alignment(index, get_u64(&full, header + 48))?);
        full.resize(at, 0);
        full.extend_from_slice(&linked[start..end]);
        set_u64(&mut full, header + 24, at as u64);
        moved.push(index);
    }
    let names_end = table + 64 * names;
    let lean_end = if moved.first() == Some(&names) {
        (get_u64(&full, names_end + 24) + get_u64(&full, names_end + 32)) as usize
    } else {
        table + 64 * sections
    };
    let mut lean = full[..lean_end].to_vec();
    for &index in moved.iter().filter(|&&index| index != names) {
        let header = table + 64 * index;
        lean[header + 4..header + 8].copy_from_slice(&SHT_NOBITS);
    }
    Ok((full, lean))
}

/// The largest `sh_addralign` [`split_kernel`] accepts for a section it
/// moves: one page. A moved section is never loaded, so no address it is
/// read at needs more; the linker gives such sections 1 to 8. A page leaves
/// room for any linker's choice and keeps the padding before each section
/// small.
const MAX_SECTION_ALIGN: u64 = 4096;

/// The alignment section `index` asks for with its `sh_addralign`, `align`.
/// gABI ch. 4, "Sections": 0 and 1 mean none, and any other value is a
/// power of two. Refuses other values and ones above [`MAX_SECTION_ALIGN`].
fn section_alignment(index: usize, align: u64) -> Result<usize> {
    if align <= 1 {
        Ok(1)
    } else if !align.is_power_of_two() {
        Err(format!(
            "section {index} has alignment {align:#x}, which is not a power of two"
        ))
    } else if align > MAX_SECTION_ALIGN {
        Err(format!(
            "section {index} has alignment {align:#x}, more than {MAX_SECTION_ALIGN:#x}"
        ))
    } else {
        Ok(align as usize)
    }
}

/// gABI ch. 4, "Sections": the type of a section that takes no space in the
/// file, as its four little-endian bytes.
const SHT_NOBITS: [u8; 4] = 8u32.to_le_bytes();

/// The end of the `size` bytes at `offset` in `file`, if they lie inside it.
fn end_in(file: &[u8], offset: u64, size: u64) -> Option<usize> {
    let end = usize::try_from(offset.checked_add(size)?).ok()?;
    (end <= file.len()).then_some(end)
}

/// Copies `from` to `to`, creating the folder `to` goes in.
fn copy_file(from: &Path, to: &Path) -> Result<()> {
    let dir = to.parent().expect("a copy target has a folder");
    fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    fs::copy(from, to).map_err(|error| format!("could not copy {}: {error}", from.display()))?;
    Ok(())
}

/// Where [`build`] puts the boot image.
fn boot_image() -> PathBuf {
    root()
        .join("target")
        .join("esp")
        .join("EFI")
        .join("BOOT")
        .join("BOOTX64.EFI")
}

/// Where [`build`] puts the kernel.
fn kernel_image() -> PathBuf {
    root()
        .join("target")
        .join("esp")
        .join("EFI")
        .join("BISMUTH")
        .join("KERNEL.ELF")
}

/// Fails unless the kernel file has at least one loadable segment and passes
/// the placement rules the boot stage uses (among them, that the entry point
/// is inside an executable segment), and describes the file.
fn describe_kernel(bytes: &[u8]) -> Result<String> {
    let elf = bismuth_elf::Elf::parse(bytes)
        .map_err(|error| format!("the ELF reader rejected the kernel: {error:?}"))?;
    let mut text = format!(
        "{:?}, entry {:#x}, {} bytes\n",
        elf.file_type(),
        elf.entry(),
        bytes.len()
    );
    for segment in elf.segments() {
        let flags = segment.flags;
        text.push_str(&format!(
            "  load {:#x}: {:#x} bytes from offset {:#x}, {:#x} in memory, align {:#x}, {}{}{}\n",
            segment.address,
            segment.file_size,
            segment.offset,
            segment.memory_size,
            segment.align,
            if flags.read { "r" } else { "-" },
            if flags.write { "w" } else { "-" },
            if flags.execute { "x" } else { "-" },
        ));
    }
    if elf.segments().next().is_none() {
        return Err(format!("the kernel has no loadable segment:\n{text}"));
    }
    match place::place(&elf) {
        Ok(_) => Ok(text),
        Err(error) => Err(format!(
            "the kernel cannot be placed as linked: {error:?}\n{text}"
        )),
    }
}

/// The pages the boot stage should fill for the kernel file `bytes`, one
/// buffer per loadable segment with memory, in file order, laid out here
/// without the placement code: each segment's contents at its address's
/// place in its first page, zeros everywhere else.
fn page_images(bytes: &[u8], elf: &bismuth_elf::Elf) -> Vec<Vec<u8>> {
    let page = place::PAGE_SIZE as usize;
    let mut images = Vec::new();
    for segment in elf.segments().filter(|s| s.memory_size > 0) {
        let start = (segment.address % place::PAGE_SIZE) as usize;
        let end = start + segment.memory_size as usize;
        let mut image = vec![0u8; end.div_ceil(page) * page];
        let offset = segment.offset as usize;
        let contents = &bytes[offset..offset + segment.file_size as usize];
        image[start..start + contents.len()].copy_from_slice(contents);
        images.push(image);
    }
    images
}

/// The line the boot stage prints after copying the kernel file `bytes`:
/// the number of segments, their pages, and the 64-bit FNV-1a sum of every
/// byte of those pages in order.
fn copied_line(bytes: &[u8], elf: &bismuth_elf::Elf) -> String {
    let images = page_images(bytes, elf);
    let mut sum = place::Fnv1a::new();
    for image in &images {
        sum.add(image);
    }
    let pages: usize = images.iter().map(|image| image.len() / 4096).sum();
    format!(
        "BISMUTH KERNEL COPIED segments={} pages={pages} sum={:#x}",
        images.len(),
        sum.value()
    )
}

/// The line the boot stage prints after giving the kernel file `bytes` its
/// addresses: the same segments, pages and sum as [`copied_line`], each
/// segment's permissions from its flags, and either state of `CR0.WP`,
/// which is the firmware's and is only reported.
fn placed_line(bytes: &[u8], elf: &bismuth_elf::Elf) -> String {
    let copied = copied_line(bytes, elf);
    let counts = copied
        .strip_prefix("BISMUTH KERNEL COPIED ")
        .expect("copied_line's prefix");
    let permissions: Vec<String> = elf
        .segments()
        .filter(|s| s.memory_size > 0)
        .map(|s| {
            let write = if s.flags.write { "w" } else { "-" };
            let execute = if s.flags.execute { "x" } else { "-" };
            format!("r{write}{execute}")
        })
        .collect();
    format!(
        "BISMUTH KERNEL PLACED {counts} perms={} wp={BIT}",
        permissions.join(",")
    )
}

/// What the boot stage writes on the firmware's console once the kernel is
/// at its addresses, under the new address tables. The firmware in QEMU
/// copies its console to the serial port, so this shows that a firmware
/// service still works after the switch.
const CONSOLE_AFTER_SWITCH: &str = "Bismuth kernel addresses checked.";

/// At the end of an expected line, stands for exactly `0` or `1`.
const BIT: &str = "?";

/// The line the boot stage prints once boot services have ended, with the
/// ranges the descriptor count and the usable pages must fall in, written
/// `[<min>..=<max>]` (see [`line_matches`]).
///
/// QEMU is given 256 MiB, 65536 pages. The OVMF build this was written
/// with reports 102 descriptors and 52559 usable pages (about 205 MiB):
/// the firmware keeps the rest for its own code and data, its runtime
/// services and its ACPI tables, and the boot stage holds the kernel file,
/// its pages and its tables. 1 to 512 descriptors allows for other
/// firmware builds. 40000 to 60000 pages (about 156 to 234 MiB) leaves
/// room either way while staying below what 256 MiB minus the firmware's
/// own several MiB could free. Counting every type instead adds the
/// firmware's memory and its device ranges: 3277728 pages there. Stepping
/// by the specification's 40 bytes instead of the firmware's stride reads
/// types and page counts from other fields: 8730 pages there.
///
/// The usable count is also named `usable`, so that the kernel's line can
/// be required to report the same number for the same boot. ` bs=0` is the
/// system table's boot services pointer after the exit, which QEMU's
/// firmware clears; the specification only says it should, so this is
/// evidence that `ExitBootServices` ran, for the test only.
fn firmware_left_line() -> String {
    "BISMUTH FIRMWARE LEFT descriptors=[1..=512] usable=[usable:40000..=60000] bs=0".to_string()
}

/// Numbers an expected line has named, for later lines of the same boot.
type Named = Vec<(String, u64)>;

/// Whether `printed` is the expected line `expected`, with no named
/// numbers known (see [`line_matches_named`]).
#[cfg(test)]
fn line_matches(expected: &str, printed: &str) -> bool {
    line_matches_named(expected, printed, &mut Named::new())
}

/// Whether `printed` is the expected line `expected`. An expected line
/// ending in [`BIT`] matches only `0` or `1` there; a `[<min>..=<max>]` in
/// one matches a decimal number in that range, and `[<name>:<min>..=<max>]`
/// also records the number in `named` under `name` if the whole line
/// matches; `[=<name>]` matches only the number recorded under `name`.
/// Everything else must be the same.
fn line_matches_named(expected: &str, printed: &str, named: &mut Named) -> bool {
    if let Some(start) = expected.strip_suffix(BIT) {
        return printed
            .strip_prefix(start)
            .is_some_and(|rest| rest == "0" || rest == "1");
    }
    let mut found = Vec::new();
    let (mut expected, mut printed) = (expected, printed);
    while let Some(open) = expected.find('[') {
        let (literal, range) = expected.split_at(open);
        let Some(after) = printed.strip_prefix(literal) else {
            return false;
        };
        let Some((bounds, rest)) = range[1..].split_once(']') else {
            return false;
        };
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let Ok(value) = after[..digits].parse::<u64>() else {
            return false;
        };
        let good = if let Some(name) = bounds.strip_prefix('=') {
            named
                .iter()
                .any(|(known, number)| known == name && *number == value)
        } else {
            let (name, bounds) = bounds.split_once(':').unwrap_or(("", bounds));
            let Some((Ok(min), Ok(max))) = bounds
                .split_once("..=")
                .map(|(min, max)| (min.parse::<u64>(), max.parse::<u64>()))
            else {
                return false;
            };
            if !name.is_empty() {
                found.push((name.to_string(), value));
            }
            (min..=max).contains(&value)
        };
        if !good {
            return false;
        }
        (expected, printed) = (rest, &after[digits..]);
    }
    let matched = printed == expected;
    if matched {
        named.extend(found);
    }
    matched
}

/// The line the boot stage prints on the serial port just before it jumps
/// to the kernel file `elf`'s entry point on the kernel's stack.
fn jump_line(elf: &bismuth_elf::Elf) -> String {
    format!(
        "BISMUTH KERNEL JUMP entry={:#x} stack={:#x} handoff={:#x}",
        elf.entry(),
        place::STACK_TOP,
        place::HANDOFF_ADDRESS
    )
}

/// The line the kernel prints once it has read the hand-off structure for
/// the kernel file `bytes`. The regions are a range: the OVMF build this
/// was written with lists 102 descriptors, which merge to 41 regions; 16
/// to 256 allows for other builds, while a list merged into a handful of
/// regions falls outside. The usable pages
/// must be the boot stage's count for the same boot, made by different
/// code from the same map. The kernel's own pages are computed here: the
/// segments' pages as [`page_images`] lays them out, the stack's pages and
/// the block of table frames.
fn memory_line(bytes: &[u8], elf: &bismuth_elf::Elf) -> String {
    let pages: u64 = page_images(bytes, elf)
        .iter()
        .map(|image| (image.len() / 4096) as u64)
        .sum();
    format!(
        "BISMUTH KERNEL MEMORY regions=[16..=256] usable=[=usable] kernel={}",
        pages + place::STACK_PAGES + place::TABLE_FRAMES
    )
}

/// A boot of the kernel file `kernel` that gets as far as the jump: the
/// `FILE OK`, `COPIED`, `PLACED` and console lines, the `FIRMWARE LEFT`
/// line, then the `JUMP` line, then the kernel's tables line, its `MEMORY`
/// line and `last` from the kernel, and QEMU exiting with `exit_code`.
fn started(name: &'static str, kernel: &[u8], last: &str, exit_code: i32) -> Result<Scenario> {
    let elf = bismuth_elf::Elf::parse(kernel)
        .map_err(|error| format!("the ELF reader rejected the {name} kernel: {error:?}"))?;
    let ok = format!(
        "BISMUTH KERNEL FILE OK entry={:#x} segments={} bytes={}",
        elf.entry(),
        elf.segments().count(),
        kernel.len()
    );
    let mut scenario = Scenario::new(name, Some(kernel), ok, exit_code);
    scenario.lines.extend([
        copied_line(kernel, &elf),
        placed_line(kernel, &elf),
        CONSOLE_AFTER_SWITCH.to_string(),
        firmware_left_line(),
        jump_line(&elf),
        TABLES_MARKER.to_string(),
        memory_line(kernel, &elf),
        last.to_string(),
    ]);
    Ok(scenario)
}

/// The offsets in `kernel` of its loadable segments' program headers. gABI
/// ch. 4, "ELF Header": `e_phoff` is the eight bytes at 32 and `e_phnum` the
/// two at 56; ch. 5, "Program Header": a header is 56 bytes and starts with
/// its type, 1 for a loadable segment.
fn load_headers(kernel: &[u8]) -> Result<Vec<usize>> {
    let table = u64::from_le_bytes(kernel[32..40].try_into().expect("eight bytes"));
    let table = usize::try_from(table).map_err(|_| "the kernel's table offset is too large")?;
    let count = u16::from_le_bytes([kernel[56], kernel[57]]);
    Ok((0..usize::from(count))
        .map(|i| table + 56 * i)
        .filter(|&at| kernel.get(at..at + 4) == Some(&1u32.to_le_bytes()[..]))
        .collect())
}

/// Reads the eight-byte little-endian field at `at`.
fn get_u64(file: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(file[at..at + 8].try_into().expect("eight bytes"))
}

/// Writes the eight-byte little-endian field at `at`.
fn set_u64(file: &mut [u8], at: usize, value: u64) {
    file[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Runs the ELF reader on the kernel file [`build`] produced, and checks
/// that the full kernel file it left next to the build loads the same pages.
fn check_kernel() -> Result<()> {
    let read = |path: PathBuf| {
        fs::read(&path).map_err(|error| format!("could not read {}: {error}", path.display()))
    };
    let lean = read(kernel_image())?;
    print!("kernel file check passed: {}", describe_kernel(&lean)?);
    // The build that put the lean file on the disk left the full one here.
    let full = read(full_kernel(&root()))?;
    let parse = |bytes| {
        bismuth_elf::Elf::parse(bytes)
            .map_err(|error| format!("the ELF reader rejected the kernel: {error:?}"))
    };
    let images = page_images(&lean, &parse(&lean)?);
    if images != page_images(&full, &parse(&full)?) {
        return Err("the lean kernel file loads other pages than the full one".into());
    }
    let pages: usize = images.iter().map(|image| image.len() / 4096).sum();
    println!(
        "the lean kernel file, {} bytes, and the full one, {} bytes, load the same {pages} pages",
        lean.len(),
        full.len()
    );
    Ok(())
}

/// Builds the normal kernel in this repository and in a copy of it in a
/// folder of the system's temporary folder, and fails unless the linker's,
/// the full and the lean kernel files are the same bytes in both. Returns a
/// note for the check's summary line.
fn check_reproducible() -> Result<String> {
    let here = root();
    let there = env::temp_dir().join(format!("bismuth-reproducible-{}", std::process::id()));
    if there.exists() {
        fs::remove_dir_all(&there)
            .map_err(|error| format!("could not clear {}: {error}", there.display()))?;
    }
    let compare = || -> Result<String> {
        copy_tree(&here, &there, &["target", ".git"])?;
        build_kernel_in(&here, &[])?;
        build_kernel_in(&there, &[])?;
        let read = |path: PathBuf| {
            fs::read(&path).map_err(|error| format!("could not read {}: {error}", path.display()))
        };
        let mut sizes = Vec::new();
        for (name, path) in [
            ("linker's", linked_kernel as fn(&Path) -> PathBuf),
            ("full", full_kernel),
            ("lean", lean_kernel),
        ] {
            let (mine, copy) = (read(path(&here))?, read(path(&there))?);
            if mine != copy {
                return Err(format!(
                    "the {name} kernel file differs when built in {}: {} bytes here, {} there",
                    there.display(),
                    mine.len(),
                    copy.len()
                ));
            }
            sizes.push(format!("{name} {} bytes", mine.len()));
        }
        Ok(format!(
            "the same kernel files from another folder ({})",
            sizes.join(", ")
        ))
    };
    let result = compare();
    // A leftover only takes space; the next run clears it.
    let _ = fs::remove_dir_all(&there);
    result
}

/// Copies the folder `from` to `to`, all but the entries of `from` itself
/// named in `skip`.
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> Result<()> {
    fs::create_dir_all(to)
        .map_err(|error| format!("could not create {}: {error}", to.display()))?;
    let entries = fs::read_dir(from)
        .map_err(|error| format!("could not list {}: {error}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not list {}: {error}", from.display()))?;
        if skip.iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target, &[])?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Builds the boot image as `cargo xtask build` does and measures the
/// footprint report.
///
/// It builds again rather than reusing `target/esp`, because the boot test
/// leaves a `qemu-exit` build there.
fn footprint_report() -> Result<String> {
    build(false)?;
    Ok(footprint::report(&footprint::measure(
        &root(),
        &boot_image(),
        &kernel_image(),
    )?))
}

/// Prints the footprint report, and with `update` stores it as the expected
/// one.
fn snapshot(update: bool) -> Result<()> {
    let report = footprint_report()?;
    print!("{report}");
    if update {
        let path = root().join(footprint::STORED);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
        }
        fs::write(&path, &report)
            .map_err(|error| format!("could not write {}: {error}", footprint::STORED))?;
        println!("stored in {}", footprint::STORED);
    }
    Ok(())
}

/// Fails, showing the difference, if the footprint report does not match the
/// stored one.
fn check_snapshot() -> Result<()> {
    let report = footprint_report()?;
    let stored = fs::read_to_string(root().join(footprint::STORED)).map_err(|error| {
        format!(
            "could not read {}: {error}. Run `cargo xtask snapshot --update` to create it",
            footprint::STORED
        )
    })?;
    match footprint::difference(&stored, &report) {
        None => {
            println!("footprint snapshot matches {}", footprint::STORED);
            Ok(())
        }
        Some(difference) => Err(format!(
            "the footprint differs from {stored_path} (- stored, + now):\n{difference}\
             If the change is intended, run `cargo xtask snapshot --update`, commit \
             {stored_path} and explain each changed line in the pull request",
            stored_path = footprint::STORED
        )),
    }
}

/// Boots the image in QEMU with a window, serial output in this terminal.
fn run() -> Result<()> {
    build(false)?;
    let mut qemu = qemu_command("target/esp")?;
    qemu.args(["-serial", "stdio"]);
    run_checked(&mut qemu, "QEMU")
}

/// One boot of the boot stage in QEMU, on a disk of its own.
struct Scenario {
    /// The name printed with the result, also the disk's folder name.
    name: &'static str,
    /// The kernel file to put on the disk, or `None` for no file.
    kernel: Option<Vec<u8>>,
    /// Lines the serial output must contain, in this order.
    lines: Vec<String>,
    /// Lines the serial output must not contain anywhere.
    absent: Vec<String>,
    /// The exit code QEMU must end with.
    exit_code: i32,
    /// The processor model QEMU imitates (its `-cpu` argument), or `None`
    /// for QEMU's default.
    cpu: Option<&'static str>,
    /// The kernel's fault report the serial output must contain, if any.
    fault: Option<FaultLine>,
    /// The boot image to put on the disk, or `None` for the one in
    /// `target/esp`.
    boot: Option<PathBuf>,
}

impl Scenario {
    fn new(name: &'static str, kernel: Option<&[u8]>, line: String, exit_code: i32) -> Self {
        Scenario {
            name,
            kernel: kernel.map(<[u8]>::to_vec),
            lines: vec![BOOT_MARKER.to_string(), line],
            absent: Vec::new(),
            exit_code,
            cpu: None,
            fault: None,
            boot: None,
        }
    }

    fn error(name: &'static str, kernel: Option<&[u8]>, reason: &str) -> Self {
        let line = format!("BISMUTH KERNEL FILE ERROR: {reason}");
        Scenario::new(name, kernel, line, QEMU_FAILURE_EXIT)
    }

    fn place_error(name: &'static str, kernel: &[u8], reason: &str) -> Self {
        let line = format!("BISMUTH KERNEL PLACE ERROR: {reason}");
        Scenario::new(name, Some(kernel), line, QEMU_FAILURE_EXIT)
    }
}

/// The boot scenarios, built from the real kernel file `kernel`.
///
/// The normal one expects the entry point and loadable segment count that
/// the ELF reader finds in the same file here, as [`check_kernel`] reports,
/// and the sum of the pages [`page_images`] lays out. The last three change
/// program headers so the file still reads but cannot be placed.
fn scenarios(kernel: &[u8]) -> Result<Vec<Scenario>> {
    let elf = bismuth_elf::Elf::parse(kernel)
        .map_err(|error| format!("the ELF reader rejected the kernel: {error:?}"))?;
    let ok = format!(
        "BISMUTH KERNEL FILE OK entry={:#x} segments={} bytes={}",
        elf.entry(),
        elf.segments().count(),
        kernel.len()
    );
    let mut bad_signature = kernel.to_vec();
    bad_signature[1] = b'X';
    // Cut in the middle of the last program header. gABI ch. 4, "ELF
    // Header": `e_phoff` is the eight bytes at 32 and `e_phnum` the two at
    // 56; a program header is 56 bytes.
    let table = u64::from_le_bytes(kernel[32..40].try_into().expect("eight bytes"));
    let count = u16::from_le_bytes([kernel[56], kernel[57]]);
    let table = usize::try_from(table).map_err(|_| "the kernel's table offset is too large")?;
    let cut = table + 56 * usize::from(count.max(1) - 1) + 28;
    let truncated = kernel
        .get(..cut)
        .ok_or("the kernel is too short to truncate")?;
    let normal = started("normal", kernel, KERNEL_MARKER, KERNEL_SUCCESS_EXIT)?;

    // gABI ch. 4, "ELF Header": `e_entry` is the eight bytes at 24; ch. 5,
    // "Program Header": `p_vaddr` is at 16 and `p_memsz` at 40 in a header.
    let loads = load_headers(kernel)?;
    let (Some(&first), Some(&second), Some(&last)) = (loads.first(), loads.get(1), loads.last())
    else {
        return Err("the kernel needs two loadable segments for the boot scenarios".into());
    };
    // Moved by a whole number of pages, so the ELF reader's alignment check
    // still passes.
    let low = |address: u64| address.wrapping_sub(place::KERNEL_BASE) + 0x20_0000;
    let mut linked_low = kernel.to_vec();
    set_u64(&mut linked_low, 24, low(get_u64(kernel, 24)));
    for &at in &loads {
        set_u64(&mut linked_low, at + 16, low(get_u64(kernel, at + 16)));
    }
    // The second segment moved onto the first one's first page, at the
    // same place in the page.
    let mut shared_page = kernel.to_vec();
    let address = get_u64(kernel, first + 16) & !0xFFF | get_u64(kernel, second + 16) & 0xFFF;
    set_u64(&mut shared_page, second + 16, address);
    // The last segment grown to the whole limit on its own.
    let mut too_large = kernel.to_vec();
    set_u64(
        &mut too_large,
        last + 40,
        place::PAGE_LIMIT * place::PAGE_SIZE,
    );
    let grown = bismuth_elf::Elf::parse(&too_large)
        .map_err(|error| format!("the ELF reader rejected the grown kernel: {error:?}"))?;
    let pages: usize = page_images(&too_large, &grown)
        .iter()
        .map(|image| image.len() / 4096)
        .sum();
    // The last segment grown by two pages it does not fill from the file,
    // so it takes three or more: every page must be cleared, copied,
    // summed and mapped, not only the first.
    let mut long = kernel.to_vec();
    set_u64(
        &mut long,
        last + 40,
        get_u64(kernel, last + 40) + 2 * place::PAGE_SIZE,
    );
    let long_segment = started("long-segment", &long, KERNEL_MARKER, KERNEL_SUCCESS_EXIT)?;
    // The last segment moved onto the last page of the address range, at
    // the same place in the page.
    let mut top_page = kernel.to_vec();
    set_u64(
        &mut top_page,
        last + 16,
        get_u64(kernel, last + 16) | !(place::PAGE_SIZE - 1),
    );
    // And cut to the bytes it has in the file (`p_filesz`, at 32), so it
    // still ends inside that page, which the ELF reader requires: the
    // kernel's writable segment is mostly zeros that take no file space.
    set_u64(&mut top_page, last + 40, get_u64(kernel, last + 32));
    // The real kernel on a processor without no-execute pages: the
    // `qemu64` model, QEMU's default for this machine, with the feature
    // (CPUID 80000001h, EDX bit 20) taken away.
    let mut no_execute = Scenario::new("no-execute", Some(kernel), ok.clone(), QEMU_FAILURE_EXIT);
    no_execute.lines.push(copied_line(kernel, &elf));
    no_execute
        .lines
        .push("BISMUTH KERNEL MAP ERROR: the processor cannot mark pages no-execute".into());
    no_execute.cpu = Some(NO_EXECUTE_CPU);
    no_execute.absent.push(CONSOLE_AFTER_SWITCH.to_string());
    // Entered at the first byte of the first segment, which the kernel
    // linker makes read-only.
    let mut entry_not_code = kernel.to_vec();
    set_u64(&mut entry_not_code, 24, get_u64(kernel, first + 16));

    Ok(vec![
        normal,
        Scenario::error("missing", None, "file not found"),
        Scenario::error("empty", Some(&[]), "file is empty"),
        Scenario::error(
            "bad-signature",
            Some(&bad_signature),
            "ELF reader: BadSignature",
        ),
        Scenario::error(
            "truncated",
            Some(truncated),
            "ELF reader: ProgramHeadersOutsideFile",
        ),
        Scenario::place_error(
            "linked-low",
            &linked_low,
            "entry point below 0xffffffff80000000",
        ),
        Scenario::place_error("shared-page", &shared_page, "segments 0 and 1 share a page"),
        Scenario::place_error(
            "too-large",
            &too_large,
            &format!(
                "{pages} pages, more than the limit of {}",
                place::PAGE_LIMIT
            ),
        ),
        long_segment,
        Scenario::place_error(
            "entry-not-code",
            &entry_not_code,
            "entry point not in an executable segment",
        ),
        Scenario::place_error(
            "top-page",
            &top_page,
            &format!(
                "segment {} uses the last page of the address range",
                loads.len() - 1
            ),
        ),
        no_execute,
    ])
}

/// The kernel built with its `panic-at-entry` feature, `panicking`: started
/// like the normal kernel, it prints the panic line and QEMU exits with the
/// kernel's failure code.
fn panic_scenario(panicking: &[u8]) -> Result<Scenario> {
    let mut scenario = started("kernel-panic", panicking, KERNEL_PANIC, KERNEL_FAILURE_EXIT)?;
    scenario.absent.push(KERNEL_MARKER.to_string());
    Ok(scenario)
}

/// The kernel's report of a processor exception, as a fault scenario
/// expects it: `BISMUTH KERNEL FAULT vector=<n> <name> error=0x<code>
/// rip=0x<address> rsp=0x<address>`, and for a page fault ` cr2=0x<address>`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FaultLine {
    /// Everything up to and including the error code, exactly.
    start: String,
    /// Where the reported instruction must be, or `None` where the manual
    /// leaves it undefined (a double fault).
    rip: Option<Range<u64>>,
    /// Where the reported stack pointer must be.
    rsp: Range<u64>,
    /// Where the address in CR2 must be, for a page fault; `None` means the
    /// line must have no `cr2`.
    cr2: Option<Range<u64>>,
}

/// How every fault report starts.
const FAULT_MARKER: &str = "BISMUTH KERNEL FAULT ";

impl FaultLine {
    /// Says why the kernel's fault report `line` is not this one, or `None`
    /// if it is.
    fn mismatch(&self, line: &str) -> Option<String> {
        let Some(rest) = line.strip_prefix(&self.start) else {
            return Some(format!("{line:?} does not start {:?}", self.start));
        };
        let mut wanted = vec![("rip", self.rip.clone()), ("rsp", Some(self.rsp.clone()))];
        if let Some(cr2) = &self.cr2 {
            wanted.push(("cr2", Some(cr2.clone())));
        }
        let fields: Vec<&str> = rest.split(' ').skip(1).collect();
        if !rest.starts_with(' ') || fields.len() != wanted.len() {
            return Some(format!("{line:?} does not have the fields {wanted:x?}"));
        }
        for ((name, range), field) in wanted.into_iter().zip(fields) {
            let value = field
                .strip_prefix(name)
                .and_then(|field| field.strip_prefix("=0x"))
                .and_then(|digits| u64::from_str_radix(digits, 16).ok());
            match (value, range) {
                (None, _) => return Some(format!("{field:?} is not {name}=0x<hexadecimal>")),
                (Some(value), Some(range)) if !range.contains(&value) => {
                    return Some(format!("{name}={value:#x} is outside {range:#x?}"));
                }
                _ => {}
            }
        }
        None
    }
}

/// One deliberate fault the kernel can be built to make: the kernel feature
/// that makes it, the scenario's name, and what the report must say.
struct DeliberateFault {
    feature: &'static str,
    vector: u8,
    name: &'static str,
    error: u64,
    /// Where the instruction is: in the code, in the writable data, or
    /// undefined.
    rip: Place,
    /// Where the stack pointer is.
    rsp: Range<u64>,
    /// Where the touched address is, for a page fault.
    cr2: Option<Place>,
}

/// A part of the kernel's address range a report is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The segment that may be run.
    Code,
    /// The segment that may only be read.
    ReadOnly,
    /// The segment that may be written.
    Writable,
    /// The unmapped page below the stack.
    Guard,
    /// Anywhere: the manual leaves it undefined.
    Undefined,
}

/// The kernel's stack, which the boot stage maps.
const KERNEL_STACK: Range<u64> = place::STACK_BOTTOM..place::STACK_TOP;
/// The kernel's stack and its guard page below, where the stack pointer is
/// when the stack has overflowed.
const OVERFLOWED_STACK: Range<u64> = place::STACK_GUARD..place::STACK_BOTTOM + place::PAGE_SIZE;

/// The faults the kernel's `fault-*` features make, in the order their
/// scenarios run. Page-fault error codes are SDM vol. 3A, "Interrupt
/// 14—Page-Fault Exception (#PF)": bit 0 the page was present, bit 1 a
/// write, bit 4 an instruction fetch.
const DELIBERATE_FAULTS: [DeliberateFault; 7] = [
    DeliberateFault {
        feature: "fault-divide",
        vector: 0,
        name: "#DE",
        error: 0,
        rip: Place::Code,
        rsp: KERNEL_STACK,
        cr2: None,
    },
    DeliberateFault {
        feature: "fault-invalid-opcode",
        vector: 6,
        name: "#UD",
        error: 0,
        rip: Place::Code,
        rsp: KERNEL_STACK,
        cr2: None,
    },
    // A present page, written.
    DeliberateFault {
        feature: "fault-write-read-only",
        vector: 14,
        name: "#PF",
        error: 0x3,
        rip: Place::Code,
        rsp: KERNEL_STACK,
        cr2: Some(Place::ReadOnly),
    },
    // A present page, fetched from: the instruction is the address itself.
    DeliberateFault {
        feature: "fault-run-data",
        vector: 14,
        name: "#PF",
        error: 0x11,
        rip: Place::Writable,
        rsp: KERNEL_STACK,
        cr2: Some(Place::Writable),
    },
    // A missing page, read.
    DeliberateFault {
        feature: "fault-guard-page",
        vector: 14,
        name: "#PF",
        error: 0,
        rip: Place::Code,
        rsp: KERNEL_STACK,
        cr2: Some(Place::Guard),
    },
    // The page fault cannot be delivered on the overflowed stack, so the
    // processor raises a double fault, on its own stack, with error code 0
    // and the saved instruction undefined.
    DeliberateFault {
        feature: "fault-stack-overflow",
        vector: 8,
        name: "#DF",
        error: 0,
        rip: Place::Undefined,
        rsp: OVERFLOWED_STACK,
        cr2: None,
    },
    // `int 255`: the last gate, reached by software, to the default handler.
    DeliberateFault {
        feature: "fault-last-vector",
        vector: 255,
        name: "interrupt",
        error: 0,
        rip: Place::Code,
        rsp: KERNEL_STACK,
        cr2: None,
    },
];

/// The scenario for the kernel file `kernel`, built with `fault`'s feature:
/// started like the normal kernel, it loads its tables and reads the
/// hand-off, then makes the fault and reports it with its own line, and QEMU exits with the kernel's
/// failure code. The firmware's handler must not run, nor the kernel's
/// normal end.
fn fault_scenario(fault: &DeliberateFault, kernel: &[u8]) -> Result<Scenario> {
    let elf = bismuth_elf::Elf::parse(kernel).map_err(|error| {
        format!(
            "the ELF reader rejected the {} kernel: {error:?}",
            fault.feature
        )
    })?;
    let range = |place: Place| -> Result<Range<u64>> {
        let wanted = |flags: bismuth_elf::Flags| match place {
            Place::Code => flags.execute,
            Place::ReadOnly => !flags.write && !flags.execute,
            Place::Writable => flags.write,
            Place::Guard | Place::Undefined => false,
        };
        match place {
            Place::Guard => return Ok(place::STACK_GUARD..place::STACK_BOTTOM),
            Place::Undefined => return Ok(0..u64::MAX),
            _ => {}
        }
        let mut segments = elf
            .segments()
            .filter(|segment| segment.memory_size > 0 && wanted(segment.flags));
        match (segments.next(), segments.next()) {
            (Some(segment), None) => Ok(segment.address..segment.address + segment.memory_size),
            _ => Err(format!(
                "the {} kernel needs exactly one {place:?} segment",
                fault.feature
            )),
        }
    };
    // The lines up to the kernel's `MEMORY` line; `started` adds one more.
    let mut scenario = started(fault.feature, kernel, "", KERNEL_FAILURE_EXIT)?;
    scenario.lines.pop();
    scenario.fault = Some(FaultLine {
        start: format!(
            "BISMUTH KERNEL FAULT vector={} {} error={:#x}",
            fault.vector, fault.name, fault.error
        ),
        rip: (fault.rip != Place::Undefined)
            .then(|| range(fault.rip))
            .transpose()?,
        rsp: fault.rsp.clone(),
        cr2: fault.cr2.map(range).transpose()?,
    });
    scenario.absent.push(KERNEL_MARKER.to_string());
    Ok(scenario)
}

/// The real kernel file `kernel`, started by the boot image `boot`, built
/// with the boot stage's `damage-handoff` feature, which damages the hand-off
/// structure's marker after writing it: the kernel refuses the structure
/// and stops with its failure code.
fn damaged_handoff_scenario(kernel: &[u8], boot: PathBuf) -> Result<Scenario> {
    let mut scenario = started(
        "damaged-handoff",
        kernel,
        "BISMUTH KERNEL HANDOFF ERROR: the marker is wrong",
        KERNEL_FAILURE_EXIT,
    )?;
    // The kernel's `MEMORY` line is not printed.
    scenario.lines.remove(scenario.lines.len() - 2);
    scenario.absent.push(KERNEL_MARKER.to_string());
    scenario.boot = Some(boot);
    Ok(scenario)
}

/// Where the boot image built with `damage-handoff` is kept for its
/// scenario.
const DAMAGED_BOOT: &str = "target/boot-damaged/BOOTX64.EFI";

/// The processor model for the scenario without no-execute pages.
const NO_EXECUTE_CPU: &str = "qemu64,-nx";

/// Builds the boot stage and the kernel with `qemu-exit`, runs every boot
/// scenario and fails if any did not behave as expected. After the
/// scenarios of the real kernel file come one with a kernel file built to
/// panic, one with a boot image built to damage the hand-off structure,
/// and one per deliberate fault, each with a kernel file of its own.
fn test() -> Result<()> {
    let damaged = root().join(DAMAGED_BOOT);
    copy_file(&build_boot(&["qemu-exit", "damage-handoff"])?, &damaged)?;
    build(true)?;
    let kernel = fs::read(kernel_image())
        .map_err(|error| format!("could not read the kernel file: {error}"))?;
    let panicking = build_kernel(&["qemu-exit", "panic-at-entry"])?;
    let panicking = fs::read(&panicking)
        .map_err(|error| format!("could not read the panicking kernel file: {error}"))?;
    let mut all = scenarios(&kernel)?;
    all.push(panic_scenario(&panicking)?);
    all.push(damaged_handoff_scenario(&kernel, damaged)?);
    for fault in &DELIBERATE_FAULTS {
        let file = build_kernel(&["qemu-exit", fault.feature])?;
        let file = fs::read(&file).map_err(|error| {
            format!("could not read the {} kernel file: {error}", fault.feature)
        })?;
        all.push(fault_scenario(fault, &file)?);
    }
    let mut failed = Vec::new();
    for scenario in all {
        let started = Instant::now();
        let disk = scenario_disk(&scenario)?;
        let (status, serial) = boot(&disk, scenario.cpu)?;
        let seconds = started.elapsed().as_secs_f32();
        if let Some(reason) = boot_failure(&scenario, status, &serial) {
            eprintln!(
                "boot scenario {} FAILED after {seconds:.1} s: {reason}\n\
                 --- serial output ---\n{}\n--- end ---",
                scenario.name,
                serial.trim_end()
            );
            failed.push(scenario.name);
            continue;
        }
        println!(
            "boot scenario {} passed in {seconds:.1} s, exit code {}:",
            scenario.name, scenario.exit_code
        );
        for line in &scenario.lines {
            println!("  {line}");
        }
        if scenario.fault.is_some() {
            let mut lines = serial.lines().map(|line| line.trim_end_matches('\r'));
            if let Some(line) = lines.find(|line| line.starts_with(FAULT_MARKER)) {
                println!("  {line}");
            }
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("boot scenarios failed: {}", failed.join(", ")))
    }
}

/// Lays out the scenario's disk: the boot image from `target/esp`, and its
/// kernel file if it has one. Returns the folder, relative to the root.
fn scenario_disk(scenario: &Scenario) -> Result<String> {
    let disk = format!("{SCENARIO_DISKS}/{}", scenario.name);
    let dir = root().join(&disk);
    if dir.exists() {
        fs::remove_dir_all(&dir)
            .map_err(|error| format!("could not clear {}: {error}", dir.display()))?;
    }
    let efi = dir.join("EFI");
    let boot = scenario.boot.clone().unwrap_or_else(boot_image);
    copy_file(&boot, &efi.join("BOOT").join("BOOTX64.EFI"))?;
    if let Some(kernel) = &scenario.kernel {
        let folder = efi.join("BISMUTH");
        fs::create_dir_all(&folder)
            .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
        fs::write(folder.join("KERNEL.ELF"), kernel)
            .map_err(|error| format!("could not write the kernel into {disk}: {error}"))?;
    }
    Ok(disk)
}

/// What the firmware in QEMU (OVMF) prints on the serial port when the
/// processor raises an exception it handles, such as a page fault in the
/// kernel, before it stops the processor for good.
const FIRMWARE_EXCEPTION: &str = "!!!! X64 Exception Type";

/// Says why a boot that ended with exit code `status` (`None` for a hang)
/// and printed `serial` does not meet `scenario`, or `None` if it does.
fn boot_failure(scenario: &Scenario, status: Option<i32>, serial: &str) -> Option<String> {
    let Some(code) = status else {
        if let Some(line) = serial
            .lines()
            .find(|line| line.contains(FIRMWARE_EXCEPTION))
        {
            return Some(format!(
                "the firmware reported a processor exception and QEMU was stopped: {}",
                line.trim()
            ));
        }
        return Some(format!(
            "QEMU did not exit within {} seconds",
            BOOT_TIMEOUT.as_secs()
        ));
    };
    if code != scenario.exit_code {
        return Some(format!(
            "QEMU exited with code {code}, expected {}",
            scenario.exit_code
        ));
    }
    let mut printed = serial.lines().map(|line| line.trim_end_matches('\r'));
    let mut named = Named::new();
    for line in &scenario.lines {
        if !printed.any(|printed| line_matches_named(line, printed, &mut named)) {
            return Some(format!("serial output did not contain {line:?} in order"));
        }
    }
    let mut all = serial.lines().map(|line| line.trim_end_matches('\r'));
    if let Some(line) = all.find(|line| scenario.absent.iter().any(|absent| absent == line)) {
        return Some(format!("serial output contained {line:?}"));
    }
    let fault = scenario.fault.as_ref()?;
    let mut all = serial.lines().map(|line| line.trim_end_matches('\r'));
    match all.find(|line| line.starts_with(FAULT_MARKER)) {
        Some(line) => fault.mismatch(line),
        None => Some(format!(
            "serial output did not contain a {FAULT_MARKER:?}line"
        )),
    }
}

/// Boots the folder `disk` in QEMU with no window, imitating processor model
/// `cpu` if one is given. Returns QEMU's exit code, or `None` if it ran past
/// [`BOOT_TIMEOUT`] or was stopped by a signal, and the serial output.
fn boot(disk: &str, cpu: Option<&str>) -> Result<(Option<i32>, String)> {
    let mut qemu = qemu_command(disk)?;
    if let Some(cpu) = cpu {
        qemu.args(["-cpu", cpu]);
    }
    qemu.args(["-serial", "stdio", "-display", "none", "-no-reboot"])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());

    let mut child = qemu
        .spawn()
        .map_err(|error| format!("could not start QEMU: {error}"))?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let captured = Arc::clone(&captured);
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            // A read error only shortens the captured output; the checks
            // below still decide the result.
            while let Ok(read @ 1..) = stdout.read(&mut chunk) {
                if let Ok(mut bytes) = captured.lock() {
                    bytes.extend_from_slice(&chunk[..read]);
                }
            }
        })
    };
    let faulted = || {
        captured.lock().is_ok_and(|bytes| {
            bytes
                .windows(FIRMWARE_EXCEPTION.len())
                .any(|window| window == FIRMWARE_EXCEPTION.as_bytes())
        })
    };

    let deadline = Instant::now() + BOOT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            // The firmware's handler stops the processor for good after
            // its report, so waiting longer would only reach the limit.
            Ok(None) if Instant::now() >= deadline || faulted() => {
                // Killing can only fail if QEMU has just exited by itself.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("could not wait for QEMU: {error}")),
        }
    };
    let _ = reader.join();
    let bytes = captured
        .lock()
        .map(|bytes| bytes.clone())
        .unwrap_or_default();
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// How a step of `check` that did not pass ended.
#[derive(Debug)]
enum Stop {
    /// The step ran and failed, for this reason.
    Failed(String),
    /// The step needs QEMU, which is not installed. Says where it looked.
    QemuMissing(String),
}

/// One step of `check`: its name, and what runs it. On success it returns a
/// note for the step's summary line, empty for none.
type Step<'a> = (
    &'static str,
    Box<dyn FnOnce() -> std::result::Result<String, Stop> + 'a>,
);

/// What `check` exits with when nothing failed but a step could not run
/// because QEMU is missing. 1 is a failure and 2 a usage error.
const CHECK_NOT_RUN_EXIT: u8 = 3;

/// Runs `steps` in order, sending a summary line through `report` as each
/// one finishes, and a last line saying how the check ended. Returns the
/// exit code.
///
/// Each step's line starts with `check: `, so the lines can be picked out of
/// the tools' own output. The last line is the verdict.
///
/// The first failing step ends the check; the steps after it do not run. A
/// step that finds QEMU missing is a failure if `require_qemu`; otherwise the
/// rest still run, and unless one fails, the last line says what did not run.
fn run_steps(steps: Vec<Step>, require_qemu: bool, report: &mut dyn FnMut(&str)) -> u8 {
    let mut not_run = None;
    for (name, step) in steps {
        let reason = match step() {
            Ok(note) if note.is_empty() => {
                report(&format!("check: {name}: ok"));
                continue;
            }
            Ok(note) => {
                report(&format!("check: {name}: ok, {note}"));
                continue;
            }
            Err(Stop::QemuMissing(looked)) if !require_qemu => {
                report(&format!("check: {name}: NOT RUN: {looked}"));
                not_run.get_or_insert(name);
                continue;
            }
            Err(Stop::QemuMissing(looked)) => {
                format!("QEMU is not installed and BISMUTH_REQUIRE_QEMU is set: {looked}")
            }
            Err(Stop::Failed(reason)) => reason,
        };
        report(&format!("check: {name}: FAILED: {reason}"));
        report(&format!("check FAILED at {name}"));
        return 1;
    }
    match not_run {
        None => {
            report("check passed");
            0
        }
        Some(name) => {
            report(&format!("{name} NOT RUN: QEMU is not installed"));
            CHECK_NOT_RUN_EXIT
        }
    }
}

/// Turns a step's plain result into a [`Step`] result with no note.
fn passed(result: Result<()>) -> std::result::Result<String, Stop> {
    result.map(|()| String::new()).map_err(Stop::Failed)
}

/// A step that runs `cargo` with `args`.
fn cargo_step<'a>(name: &'static str, args: Vec<&'static str>) -> Step<'a> {
    let what = format!("cargo {}", args.join(" "));
    (
        name,
        Box::new(move || passed(run_checked(cargo().args(&args), &what))),
    )
}

/// Runs every check the pull request pipeline runs, printing a line per step
/// and a last line saying how it ended. Exits with [`CHECK_NOT_RUN_EXIT`] if
/// only the boot test could not run, for lack of QEMU; with
/// `BISMUTH_REQUIRE_QEMU` set, that is an ordinary failure.
fn check() -> Result<()> {
    let lints = |package, rest: &[&'static str]| {
        let mut args = vec!["clippy", "--package", package];
        args.extend(rest);
        args.extend(["--", "-D", "warnings"]);
        args
    };
    let steps: Vec<Step> = vec![
        (
            "map",
            Box::new(|| {
                check_map()
                    .map(|count| format!("{count} paths"))
                    .map_err(Stop::Failed)
            }),
        ),
        cargo_step("format", vec!["fmt", "--all", "--check"]),
        cargo_step("unit tests (xtask)", vec!["test", "--package", "xtask"]),
        cargo_step("lints (xtask)", lints("xtask", &[])),
        cargo_step(
            "unit tests (acpi)",
            vec!["test", "--package", "bismuth-acpi"],
        ),
        cargo_step("lints (acpi)", lints("bismuth-acpi", &["--all-targets"])),
        cargo_step(
            "unit tests (apic)",
            vec!["test", "--package", "bismuth-apic"],
        ),
        cargo_step("lints (apic)", lints("bismuth-apic", &["--all-targets"])),
        cargo_step(
            "unit tests (descriptors)",
            vec!["test", "--package", "bismuth-descriptors"],
        ),
        cargo_step(
            "lints (descriptors)",
            lints("bismuth-descriptors", &["--all-targets"]),
        ),
        cargo_step("unit tests (elf)", vec!["test", "--package", "bismuth-elf"]),
        cargo_step("lints (elf)", lints("bismuth-elf", &["--all-targets"])),
        cargo_step(
            "unit tests (frames)",
            vec!["test", "--package", "bismuth-frames"],
        ),
        cargo_step(
            "lints (frames)",
            lints("bismuth-frames", &["--all-targets"]),
        ),
        cargo_step(
            "unit tests (handoff)",
            vec!["test", "--package", "bismuth-handoff"],
        ),
        cargo_step(
            "lints (handoff)",
            lints("bismuth-handoff", &["--all-targets"]),
        ),
        cargo_step(
            "unit tests (paging)",
            vec!["test", "--package", "bismuth-paging"],
        ),
        cargo_step(
            "lints (paging)",
            lints("bismuth-paging", &["--all-targets"]),
        ),
        cargo_step("unit tests (pci)", vec!["test", "--package", "bismuth-pci"]),
        cargo_step("lints (pci)", lints("bismuth-pci", &["--all-targets"])),
        cargo_step(
            "lints (kernel)",
            lints(KERNEL_PACKAGE, &["--target", KERNEL_TARGET]),
        ),
        cargo_step(
            "lints (kernel, qemu-exit)",
            lints(
                KERNEL_PACKAGE,
                &["--target", KERNEL_TARGET, "--features", "qemu-exit"],
            ),
        ),
        cargo_step(
            "lints (boot)",
            lints(BOOT_PACKAGE, &["--target", UEFI_TARGET]),
        ),
        cargo_step(
            "lints (boot, qemu-exit)",
            lints(
                BOOT_PACKAGE,
                &["--target", UEFI_TARGET, "--features", "qemu-exit"],
            ),
        ),
        ("footprint", Box::new(|| passed(check_snapshot()))),
        ("kernel file", Box::new(|| passed(check_kernel()))),
        (
            "reproducible kernel file",
            Box::new(|| check_reproducible().map_err(Stop::Failed)),
        ),
        (
            "boot test",
            Box::new(|| match look_for_qemu() {
                Ok(_) => passed(test()),
                Err(looked) => Err(Stop::QemuMissing(looked)),
            }),
        ),
    ];
    let require_qemu = env::var("BISMUTH_REQUIRE_QEMU").is_ok_and(|v| !v.is_empty() && v != "0");
    match run_steps(steps, require_qemu, &mut |line| println!("{line}")) {
        0 => Ok(()),
        code => std::process::exit(code.into()),
    }
}

/// A QEMU command set up to boot the folder `disk` through UEFI firmware.
///
/// It runs from the repository root so the disk can be given as a relative
/// path, which avoids quoting problems with drive letters and spaces.
fn qemu_command(disk: &str) -> Result<Command> {
    let root = root();
    let qemu = find_qemu()?;
    let firmware = find_firmware()?;
    let mut command = Command::new(qemu);
    command
        .current_dir(&root)
        .args(["-machine", "q35", "-m", "256M", "-nic", "none"])
        .arg("-drive")
        .arg(format!(
            "if=pflash,format=raw,readonly=on,file={}",
            firmware.code.display()
        ));
    if let Some(vars) = &firmware.vars {
        // The firmware writes its settings here, so give it a private copy.
        let copy = root.join("target").join("uefi-vars.fd");
        fs::copy(vars, &copy)
            .map_err(|error| format!("could not copy {}: {error}", vars.display()))?;
        command
            .arg("-drive")
            .arg("if=pflash,format=raw,file=target/uefi-vars.fd");
    }
    // QEMU presents the directory as a FAT-formatted disk.
    command
        .arg("-drive")
        .arg(format!("format=raw,file=fat:rw:{disk}"));
    Ok(command)
}

/// The name QEMU's program is started by from `PATH`.
const QEMU_NAME: &str = "qemu-system-x86_64";

/// Decides which QEMU program to run: `variable` (the value of
/// `BISMUTH_QEMU`) as given, if set; otherwise [`QEMU_NAME`] if `on_path`
/// says it starts from `PATH`; otherwise the first of the `usual` install
/// paths that exists, as marked. If none, QEMU is not installed, and the
/// error says where it looked.
fn locate_qemu(
    variable: Option<std::ffi::OsString>,
    on_path: impl FnOnce() -> bool,
    usual: &[(&Path, bool)],
) -> Result<PathBuf> {
    if let Some(path) = variable {
        return Ok(path.into());
    }
    if on_path() {
        return Ok(QEMU_NAME.into());
    }
    if let Some((path, _)) = usual.iter().find(|(_, exists)| *exists) {
        return Ok(path.to_path_buf());
    }
    let paths: Vec<String> = usual.iter().map(|(p, _)| p.display().to_string()).collect();
    Err(format!(
        "BISMUTH_QEMU is not set, {QEMU_NAME} is not on PATH, and {} does not exist",
        paths.join(" or ")
    ))
}

/// Looks for QEMU as [`locate_qemu`] says, on this machine.
fn look_for_qemu() -> Result<PathBuf> {
    let on_path = || {
        Command::new(QEMU_NAME)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    };
    let windows_default = Path::new(r"C:\Program Files\qemu\qemu-system-x86_64.exe");
    let usual = [(windows_default, windows_default.is_file())];
    locate_qemu(env::var_os("BISMUTH_QEMU"), on_path, &usual)
}

/// Finds the QEMU program: `BISMUTH_QEMU`, then `PATH`, then the default
/// Windows install folder.
fn find_qemu() -> Result<PathBuf> {
    look_for_qemu().map_err(|looked| {
        format!(
            "could not find {QEMU_NAME} ({looked}). Install QEMU, or set BISMUTH_QEMU to its path"
        )
    })
}

/// UEFI firmware files for QEMU.
struct Firmware {
    /// The firmware program itself, mapped read-only.
    code: PathBuf,
    /// The firmware's settings store, if the firmware build uses a separate one.
    vars: Option<PathBuf>,
}

/// Finds UEFI firmware: `BISMUTH_OVMF_CODE` (and optionally
/// `BISMUTH_OVMF_VARS`), then the places common QEMU packages install it.
fn find_firmware() -> Result<Firmware> {
    if let Some(code) = env::var_os("BISMUTH_OVMF_CODE") {
        return Ok(Firmware {
            code: code.into(),
            vars: env::var_os("BISMUTH_OVMF_VARS").map(PathBuf::from),
        });
    }
    const KNOWN: &[(&str, &str)] = &[
        // Debian and Ubuntu, `ovmf` package.
        (
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ),
        (
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
        ),
        // Arch, `edk2-ovmf` package.
        (
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
            "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
        ),
        // Fedora, `edk2-ovmf` package.
        (
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
            "/usr/share/edk2/ovmf/OVMF_VARS.fd",
        ),
        // Firmware bundled with QEMU itself.
        (
            "/usr/share/qemu/edk2-x86_64-code.fd",
            "/usr/share/qemu/edk2-i386-vars.fd",
        ),
        (
            "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
            "/opt/homebrew/share/qemu/edk2-i386-vars.fd",
        ),
        (
            "/usr/local/share/qemu/edk2-x86_64-code.fd",
            "/usr/local/share/qemu/edk2-i386-vars.fd",
        ),
        (
            r"C:\Program Files\qemu\share\edk2-x86_64-code.fd",
            r"C:\Program Files\qemu\share\edk2-i386-vars.fd",
        ),
    ];
    for (code, vars) in KNOWN {
        if Path::new(code).is_file() {
            let vars = Path::new(vars);
            return Ok(Firmware {
                code: code.into(),
                vars: vars.is_file().then(|| vars.to_path_buf()),
            });
        }
    }
    Err(
        "could not find UEFI firmware for QEMU. Install it (the `ovmf` package on \
         Debian and Ubuntu; it ships with QEMU on Windows and macOS), or set \
         BISMUTH_OVMF_CODE to the firmware file"
            .into(),
    )
}

/// The feature map: what exists, where it lives and how to exercise it.
const MAP_FILE: &str = "docs/map.md";

/// Fails if the feature map names a file or folder that does not exist, and
/// otherwise returns how many paths it names.
///
/// A map that points at things that have moved misleads whoever reads it, so
/// this keeps it from going stale without anyone noticing.
fn check_map() -> Result<usize> {
    let root = root();
    let text = fs::read_to_string(root.join(MAP_FILE))
        .map_err(|error| format!("could not read {MAP_FILE}: {error}"))?;
    let paths = paths_in_markdown(&text);
    let missing: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|path| !root.join(path).exists())
        .collect();
    if missing.is_empty() {
        Ok(paths.len())
    } else {
        Err(format!(
            "{MAP_FILE} names paths that do not exist: {}",
            missing.join(", ")
        ))
    }
}

/// Returns the repository paths a Markdown document names in `code spans`.
///
/// A code span counts as a path when it has no spaces and contains a `/`.
/// Commands such as `cargo xtask test` and plain names such as `efi_main` are
/// therefore ignored. Fenced code blocks are skipped.
fn paths_in_markdown(text: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Splitting on backticks puts the code spans at the odd positions.
        for span in line.split('`').skip(1).step_by(2) {
            if span.contains('/') && !span.contains(char::is_whitespace) {
                paths.push(span.trim_end_matches('/'));
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::{
        DELIBERATE_FAULTS, FaultLine, KERNEL_SUCCESS_EXIT, QEMU_SUCCESS_EXIT, Result, Scenario,
        Step, Stop, boot_failure, copied_line, damaged_handoff_scenario, describe_kernel,
        fault_scenario, get_u64, locate_qemu, page_images, panic_scenario, paths_in_markdown,
        placed_line, remap_config, run_steps, scenarios, set_u64, split_kernel,
    };
    use bismuth_elf::place::{
        Error as PlaceError, KERNEL_BASE, PAGE_LIMIT, STACK_BOTTOM, STACK_GUARD, STACK_TOP,
    };
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    fn missing() -> Scenario {
        Scenario::error("missing", None, "file not found")
    }

    #[test]
    fn a_boot_with_the_lines_in_order_and_the_right_code_passes() {
        let serial = "firmware noise\r\nBISMUTH BOOT OK\r\nconsole\r\n\
                      BISMUTH KERNEL FILE ERROR: file not found\r\n";
        assert_eq!(boot_failure(&missing(), Some(35), serial), None);
    }

    #[test]
    fn a_boot_fails_on_a_hang_a_wrong_code_or_a_missing_line() {
        let serial = "BISMUTH BOOT OK\nBISMUTH KERNEL FILE ERROR: file not found\n";
        let failure = |status, serial| boot_failure(&missing(), status, serial).expect("fails");
        assert!(failure(None, serial).contains("did not exit"));
        let fault = "BISMUTH BOOT OK\r\n!!!! X64 Exception Type - 0E(#PF - Page-Fault)  CPU \
                     Apic ID - 00000000 !!!!\r\nRIP  - FFFFFFFF80000000\r\n";
        assert_eq!(
            failure(None, fault),
            "the firmware reported a processor exception and QEMU was stopped: \
             !!!! X64 Exception Type - 0E(#PF - Page-Fault)  CPU Apic ID - 00000000 !!!!"
        );
        assert!(failure(Some(33), serial).contains("code 33, expected 35"));
        assert!(failure(Some(35), "BISMUTH BOOT OK\n").contains("did not contain"));
        // Out of order, or only part of a line.
        let reversed = "BISMUTH KERNEL FILE ERROR: file not found\nBISMUTH BOOT OK\n";
        assert!(failure(Some(35), reversed).contains("in order"));
        let partial = "BISMUTH BOOT OK\nBISMUTH KERNEL FILE ERROR: file not found here\n";
        assert!(failure(Some(35), partial).contains("did not contain"));
    }

    #[test]
    fn a_boot_fails_on_a_line_that_must_be_absent() {
        let mut scenario = missing();
        scenario.absent.push("NOT THIS".to_string());
        let good = "BISMUTH BOOT OK\nBISMUTH KERNEL FILE ERROR: file not found\n";
        assert_eq!(boot_failure(&scenario, Some(35), good), None);
        let bad = "NOT THIS\nBISMUTH BOOT OK\nBISMUTH KERNEL FILE ERROR: file not found\n";
        let failure = boot_failure(&scenario, Some(35), bad).expect("fails");
        assert!(failure.contains("contained \"NOT THIS\""), "{failure}");
    }

    #[test]
    fn an_expected_line_ending_in_a_question_mark_matches_only_0_or_1() {
        let scenario = Scenario::new("bit", None, "LINE a=1 b=?".to_string(), 33);
        for (serial, passes) in [
            ("BISMUTH BOOT OK\nLINE a=1 b=0\n", true),
            ("BISMUTH BOOT OK\nLINE a=1 b=1\r\n", true),
            ("BISMUTH BOOT OK\nLINE a=1 b=\n", false),
            ("BISMUTH BOOT OK\nLINE a=1 b=2\n", false),
            ("BISMUTH BOOT OK\nLINE a=1 b=10\n", false),
            ("BISMUTH BOOT OK\nLINE a=1 b=1 more\n", false),
            ("BISMUTH BOOT OK\nLINE a=1 b=?\n", false),
            ("BISMUTH BOOT OK\nLINE a=2 b=0\n", false),
            ("BISMUTH BOOT OK\nLINE a=1\n", false),
        ] {
            assert_eq!(
                boot_failure(&scenario, Some(33), serial).is_none(),
                passes,
                "{serial}"
            );
        }
    }

    #[test]
    fn a_range_in_an_expected_line_matches_only_a_number_inside_it() {
        let expected = "LEFT n=[1..=512] pages=[40000..=60000]";
        for (printed, passes) in [
            ("LEFT n=1 pages=40000", true),
            ("LEFT n=512 pages=60000", true),
            ("LEFT n=97 pages=52313", true),
            ("LEFT n=0 pages=52313", false),
            ("LEFT n=513 pages=52313", false),
            ("LEFT n=97 pages=39999", false),
            ("LEFT n=97 pages=60001", false),
            ("LEFT n=97 pages=", false),
            ("LEFT n= pages=52313", false),
            ("LEFT n=97 pages=52313 more", false),
            ("LEFT n=97 pages=52313x", false),
            ("LEFT m=97 pages=52313", false),
            ("LEFT n=97 pages=99999999999999999999999", false),
            ("LEFT n=[1..=512] pages=[40000..=60000]", false),
        ] {
            assert_eq!(super::line_matches(expected, printed), passes, "{printed}");
        }
        // A line without a range must be the same.
        assert!(super::line_matches("plain", "plain"));
        assert!(!super::line_matches("plain", "plainer"));
    }

    #[test]
    fn the_scenarios_damage_the_kernel_as_named() {
        let kernel = two_segment_kernel();
        let all = scenarios(&kernel).expect("scenarios");
        let names: Vec<_> = all.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "normal",
                "missing",
                "empty",
                "bad-signature",
                "truncated",
                "linked-low",
                "shared-page",
                "too-large",
                "long-segment",
                "entry-not-code",
                "top-page",
                "no-execute"
            ]
        );
        assert_eq!(
            all[0].lines[1],
            "BISMUTH KERNEL FILE OK entry=0xffffffff80001040 segments=2 bytes=8192"
        );
        assert_eq!(all[0].exit_code, KERNEL_SUCCESS_EXIT);
        assert_eq!(all[1].kernel, None);
        assert_eq!(all[2].kernel.as_deref(), Some(&[][..]));
        for (scenario, error) in [
            (&all[3], bismuth_elf::Error::BadSignature),
            (&all[4], bismuth_elf::Error::ProgramHeadersOutsideFile),
        ] {
            let file = scenario.kernel.as_deref().expect("has a file");
            assert_eq!(bismuth_elf::Elf::parse(file).map(|_| ()), Err(error));
            assert_eq!(scenario.exit_code, 35);
        }
        // The read-only page, plus the code grown to the limit on its own.
        let pages = 1 + PAGE_LIMIT;
        for (scenario, error, line) in [
            (
                &all[5],
                PlaceError::EntryBelowBase,
                "entry point below 0xffffffff80000000".to_string(),
            ),
            (
                &all[6],
                PlaceError::SharedPage {
                    first: 0,
                    second: 1,
                },
                "segments 0 and 1 share a page".to_string(),
            ),
            (
                &all[7],
                PlaceError::TooManyPages { pages },
                format!("{pages} pages, more than the limit of {PAGE_LIMIT}"),
            ),
            (
                &all[9],
                PlaceError::EntryNotExecutable,
                "entry point not in an executable segment".to_string(),
            ),
        ] {
            let file = scenario.kernel.as_deref().expect("has a file");
            let elf = bismuth_elf::Elf::parse(file).expect("the ELF reader accepts it");
            assert_eq!(bismuth_elf::place::place(&elf).map(|_| ()), Err(error));
            assert_eq!(
                scenario.lines[1],
                format!("BISMUTH KERNEL PLACE ERROR: {line}")
            );
            assert_eq!(scenario.exit_code, 35);
        }
    }

    /// A 4 KiB x86-64 ELF executable whose one program header is a loadable
    /// segment with `flags`, holding the whole file at [`KERNEL_BASE`], and
    /// whose entry is `entry`. Layouts from the gABI "ELF Header" and
    /// "Program Header" sections.
    pub(crate) fn kernel_file(flags: u32, entry: u64, headers: u16) -> Vec<u8> {
        let mut file = vec![0u8; 4096];
        let mut put = |at: usize, bytes: &[u8]| file[at..at + bytes.len()].copy_from_slice(bytes);
        put(0, &[0x7F, b'E', b'L', b'F', 2, 1, 1]);
        put(16, &2u16.to_le_bytes()); // ET_EXEC
        put(18, &62u16.to_le_bytes()); // EM_X86_64
        put(20, &1u32.to_le_bytes());
        put(24, &entry.to_le_bytes());
        put(32, &64u64.to_le_bytes());
        put(52, &64u16.to_le_bytes());
        put(54, &56u16.to_le_bytes());
        put(56, &headers.to_le_bytes());
        put(64, &1u32.to_le_bytes()); // PT_LOAD
        put(68, &flags.to_le_bytes());
        put(80, &KERNEL_BASE.to_le_bytes());
        put(96, &4096u64.to_le_bytes());
        put(104, &4096u64.to_le_bytes());
        put(112, &4096u64.to_le_bytes());
        file
    }

    #[test]
    fn a_kernel_entered_in_an_executable_segment_passes() {
        let text = describe_kernel(&kernel_file(0b101, KERNEL_BASE + 0x40, 1)).expect("passes");
        assert_eq!(
            text,
            "Executable, entry 0xffffffff80000040, 4096 bytes\n  load 0xffffffff80000000: \
             0x1000 bytes from offset 0x0, 0x1000 in memory, align 0x1000, r-x\n"
        );
    }

    #[test]
    fn a_kernel_entered_in_a_data_segment_fails() {
        let error = describe_kernel(&kernel_file(0b110, KERNEL_BASE + 0x40, 1)).expect_err("fails");
        assert!(
            error.starts_with("the kernel cannot be placed as linked: EntryNotExecutable\n"),
            "{error}"
        );
    }

    #[test]
    fn a_kernel_entered_outside_every_segment_fails() {
        let error =
            describe_kernel(&kernel_file(0b101, KERNEL_BASE + 0x1000, 1)).expect_err("fails");
        assert!(
            error.starts_with("the kernel cannot be placed as linked: EntryNotExecutable\n"),
            "{error}"
        );
    }

    #[test]
    fn a_kernel_without_loadable_segments_fails() {
        let error = describe_kernel(&kernel_file(0b101, KERNEL_BASE + 0x40, 0)).expect_err("fails");
        assert!(error.contains("no loadable segment"), "{error}");
    }

    #[test]
    fn a_file_the_reader_rejects_fails() {
        let error = describe_kernel(&[0x7F, b'E', b'L', b'F']).expect_err("fails");
        assert_eq!(error, "the ELF reader rejected the kernel: Truncated");
    }

    #[test]
    fn a_kernel_linked_low_fails_the_file_check() {
        // gABI ch. 5, "Program Header": `p_vaddr` is the eight bytes at 16.
        let mut file = kernel_file(0b101, 0x20_0040, 1);
        file[80..88].copy_from_slice(&0x20_0000u64.to_le_bytes());
        let error = describe_kernel(&file).expect_err("fails");
        assert!(
            error.starts_with("the kernel cannot be placed as linked: EntryBelowBase\n"),
            "{error}"
        );
        // Entered high but placed low.
        file[24..32].copy_from_slice(&(KERNEL_BASE + 0x40).to_le_bytes());
        let error = describe_kernel(&file).expect_err("fails");
        assert!(
            error.starts_with("the kernel cannot be placed as linked: BelowBase { segment: 0 }"),
            "{error}"
        );
    }

    /// An 8 KiB kernel file with two loadable segments of one page each, a
    /// read-only one at [`KERNEL_BASE`] and the code after it, holding the
    /// whole file. Layouts from the gABI "Program Header" section.
    fn two_segment_kernel() -> Vec<u8> {
        let mut file = kernel_file(0b100, KERNEL_BASE + 0x1040, 2);
        file.resize(0x2000, 0);
        let mut put =
            |at: usize, value: u64| file[at..at + 8].copy_from_slice(&value.to_le_bytes());
        put(120, 0x5_0000_0001); // PT_LOAD, then flags r-x
        put(128, 0x1000);
        put(136, KERNEL_BASE + 0x1000);
        put(152, 0x1000);
        put(160, 0x1000);
        put(168, 0x1000);
        // Something to sum besides the headers.
        for (i, byte) in file[0x1000..].iter_mut().enumerate() {
            *byte = i as u8;
        }
        file
    }

    /// [`two_segment_kernel`] as the linker leaves it: a third program
    /// header that loads nothing (`PT_NOTE`) with its contents after the
    /// segments, then a section standing for debug information, the
    /// section names and the section header table, which the file header
    /// points to. Layouts from the gABI "ELF Header", "Program Header" and
    /// "Sections" sections.
    fn linked_kernel_file() -> Vec<u8> {
        let mut file = two_segment_kernel();
        file[56..58].copy_from_slice(&3u16.to_le_bytes());
        set_u64(&mut file, 176, 4); // PT_NOTE, no flags
        set_u64(&mut file, 184, 0x2000);
        set_u64(&mut file, 208, 0x10);
        file.extend([0x4E; 0x10]); // The note, to 0x2010.
        file.extend([0xDB; 0x31]); // Debug information, to 0x2041.
        file.extend(LINKED_NAMES); // Section names, to 0x205E.
        let table = file.len().next_multiple_of(8);
        file.resize(table, 0);
        let section = |name: u32, kind: u32, offset: u64, size: u64| {
            let mut header = [0u8; 64];
            header[0..4].copy_from_slice(&name.to_le_bytes());
            header[4..8].copy_from_slice(&kind.to_le_bytes());
            header[24..32].copy_from_slice(&offset.to_le_bytes());
            header[32..40].copy_from_slice(&size.to_le_bytes());
            header[48..56].copy_from_slice(&1u64.to_le_bytes());
            header
        };
        let names = LINKED_NAMES.len() as u64;
        for header in [
            section(0, 0, 0, 0),
            section(1, 1, 0x1000, 0x1000), // .text, loaded
            section(7, 1, 0x2010, 0x31),   // .debug_info
            section(19, 3, 0x2041, names), // .shstrtab
            section(29, 8, 0x2010, 0x100), // .bss
        ] {
            file.extend(header);
        }
        set_u64(&mut file, 40, table as u64);
        file[58..60].copy_from_slice(&64u16.to_le_bytes());
        file[60..62].copy_from_slice(&5u16.to_le_bytes());
        file[62..64].copy_from_slice(&3u16.to_le_bytes());
        file
    }

    /// The section names of [`linked_kernel_file`].
    const LINKED_NAMES: &[u8] = b"\0.text\0.debug_info\0.shstrtab\0.bss\0";

    /// The `sh_type` and `sh_offset` of section `index` in `file`.
    fn section_at(file: &[u8], index: usize) -> (u32, u64) {
        let header = get_u64(file, 40) as usize + 64 * index;
        let kind = u32::from_le_bytes(file[header + 4..header + 8].try_into().expect("four"));
        (kind, get_u64(file, header + 24))
    }

    #[test]
    fn the_full_and_lean_kernel_files_load_the_same_pages() {
        let linked = linked_kernel_file();
        let (full, lean) = split_kernel(&linked).expect("splits");
        let full_elf = bismuth_elf::Elf::parse(&full).expect("full reads");
        let lean_elf = bismuth_elf::Elf::parse(&lean).expect("lean reads");
        assert_eq!(page_images(&full, &full_elf), page_images(&lean, &lean_elf));
        assert_eq!(copied_line(&full, &full_elf), copied_line(&lean, &lean_elf));
        assert_eq!(placed_line(&full, &full_elf), placed_line(&lean, &lean_elf));
        // The table follows the loaded contents, which end with the note.
        assert_eq!(get_u64(&full, 40), 0x2010);
        assert_eq!(full[..0x2010], lean[..0x2010]);
        // Only the table's offset in the file header differs from the
        // linker's file.
        let mut header = linked[..0x2010].to_vec();
        set_u64(&mut header, 40, 0x2010);
        assert_eq!(full[..0x2010], header[..]);
    }

    #[test]
    fn the_full_kernel_file_keeps_every_section_after_the_table() {
        let (full, _) = split_kernel(&linked_kernel_file()).expect("splits");
        // The table, five headers, then the names, then the debug section.
        assert_eq!(full.len(), 0x2010 + 5 * 64 + LINKED_NAMES.len() + 0x31);
        assert_eq!(section_at(&full, 1), (1, 0x1000));
        assert_eq!(section_at(&full, 3), (3, 0x2150));
        assert_eq!(full[0x2150..0x2150 + LINKED_NAMES.len()], *LINKED_NAMES);
        let debug = 0x2150 + LINKED_NAMES.len();
        assert_eq!(section_at(&full, 2), (1, debug as u64));
        assert!(full[debug..].iter().all(|&byte| byte == 0xDB));
        assert_eq!(section_at(&full, 4), (8, 0x2010));
    }

    #[test]
    fn the_lean_kernel_file_keeps_only_the_section_names() {
        let (full, lean) = split_kernel(&linked_kernel_file()).expect("splits");
        assert_eq!(lean.len(), 0x2150 + LINKED_NAMES.len());
        assert_eq!(lean[0x2150..], *LINKED_NAMES);
        // The debug section takes no space in the file.
        assert_eq!(section_at(&lean, 2), (8, section_at(&full, 2).1));
        assert_eq!(section_at(&lean, 3), section_at(&full, 3));
        assert!(
            !lean
                .windows(0x31)
                .any(|run| run.iter().all(|&byte| byte == 0xDB))
        );
    }

    #[test]
    fn a_kernel_file_with_a_section_partly_loaded_is_refused() {
        let mut linked = linked_kernel_file();
        let header = get_u64(&linked, 40) as usize + 64 * 2;
        set_u64(&mut linked, header + 24, 0x2008);
        let error = split_kernel(&linked).expect_err("refused");
        assert_eq!(error, "section 2 is partly loaded or outside the file");
        let mut linked = linked_kernel_file();
        let past = linked.len() as u64;
        set_u64(&mut linked, 40, past);
        let error = split_kernel(&linked).expect_err("refused");
        assert_eq!(
            error,
            "the kernel's section header table is not as the linker writes it"
        );
    }

    /// [`linked_kernel_file`] with the debug section's `sh_addralign` (gABI
    /// ch. 4, "Sections": the eight bytes at 48 of its header) set to
    /// `align`.
    fn linked_with_debug_alignment(align: u64) -> Vec<u8> {
        let mut linked = linked_kernel_file();
        let header = get_u64(&linked, 40) as usize + 64 * 2;
        set_u64(&mut linked, header + 48, align);
        linked
    }

    #[test]
    fn a_moved_section_is_placed_at_its_alignment_in_both_files() {
        // Unaligned, the debug section would follow the names at 0x2172.
        let (full, lean) = split_kernel(&linked_with_debug_alignment(16)).expect("splits");
        let unaligned = 0x2150 + LINKED_NAMES.len();
        assert_eq!(unaligned, 0x2172);
        assert_eq!(section_at(&full, 2), (1, 0x2180));
        assert_eq!(section_at(&lean, 2), (8, 0x2180));
        assert!(full[unaligned..0x2180].iter().all(|&byte| byte == 0));
        assert_eq!(full.len(), 0x2180 + 0x31);
        assert!(full[0x2180..].iter().all(|&byte| byte == 0xDB));
    }

    #[test]
    fn section_alignments_of_none_and_powers_of_two_are_accepted() {
        // gABI ch. 4, "Sections": 0 and 1 both mean no alignment.
        for (align, at) in [(0, 0x2172), (1, 0x2172), (8, 0x2178)] {
            let (full, _) = split_kernel(&linked_with_debug_alignment(align))
                .unwrap_or_else(|error| panic!("alignment {align}: {error}"));
            assert_eq!(section_at(&full, 2), (1, at), "alignment {align}");
        }
    }

    #[test]
    fn a_section_alignment_that_is_too_large_is_refused() {
        for align in [u64::MAX, 1 << 63, 1 << 40] {
            let error = split_kernel(&linked_with_debug_alignment(align)).expect_err("refused");
            let expected = if align.is_power_of_two() {
                format!("section 2 has alignment {align:#x}, more than 0x1000")
            } else {
                format!("section 2 has alignment {align:#x}, which is not a power of two")
            };
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn a_section_alignment_that_is_not_a_power_of_two_is_refused() {
        let error = split_kernel(&linked_with_debug_alignment(3)).expect_err("refused");
        assert_eq!(
            error,
            "section 2 has alignment 0x3, which is not a power of two"
        );
    }

    #[test]
    fn a_kernel_file_whose_moved_sections_share_bytes_is_refused() {
        // The debug section made to cover everything after the loaded
        // contents, the names included, which are moved first: together
        // they take more than follows the loaded contents.
        let mut linked = linked_kernel_file();
        let header = get_u64(&linked, 40) as usize + 64 * 2;
        let rest = linked.len() as u64 - 0x2010;
        set_u64(&mut linked, header + 32, rest);
        let error = split_kernel(&linked).expect_err("refused");
        assert_eq!(
            error,
            "section 2 and the sections moved before it take more bytes than follow the loaded contents"
        );
    }

    #[test]
    fn the_remap_names_the_folder_as_a_toml_string() {
        assert_eq!(
            remap_config(Path::new(r#"C:\a "b"\c"#)).expect("formats"),
            r#"target.x86_64-unknown-none.rustflags=["--remap-path-prefix=C:\\a \"b\"\\c=."]"#
        );
        assert!(remap_config(Path::new("/a\nb")).is_err());
    }

    #[test]
    fn the_normal_scenario_expects_the_sum_of_the_file_laid_out_in_pages() {
        let kernel = two_segment_kernel();
        let normal = &scenarios(&kernel).expect("scenarios")[0];
        // Both segments fill their pages straight from the file, so the
        // pages hold exactly the file.
        let mut sum = bismuth_elf::place::Fnv1a::new();
        sum.add(&kernel);
        assert_eq!(
            normal.lines[2],
            format!(
                "BISMUTH KERNEL COPIED segments=2 pages=2 sum={:#x}",
                sum.value()
            )
        );
    }

    #[test]
    fn the_placed_lines_expect_the_copied_sum_and_each_segments_permissions() {
        let kernel = two_segment_kernel();
        let all = scenarios(&kernel).expect("scenarios");
        let normal = &all[0];
        let copied = normal.lines[2]
            .strip_prefix("BISMUTH KERNEL COPIED ")
            .expect("copied");
        assert_eq!(
            normal.lines[3],
            format!("BISMUTH KERNEL PLACED {copied} perms=r--,r-x wp=?")
        );
        assert_eq!(normal.lines[4], "Bismuth kernel addresses checked.");
        // The code grown from one page to three, the two new ones zero.
        let long = &all[8];
        assert_eq!(long.exit_code, KERNEL_SUCCESS_EXIT);
        let file = long.kernel.as_deref().expect("has a file");
        // The pages hold the patched file, then zeros.
        let mut sum = bismuth_elf::place::Fnv1a::new();
        sum.add(file);
        sum.add(&[0; 0x2000]);
        assert_eq!(
            long.lines[2],
            format!(
                "BISMUTH KERNEL COPIED segments=2 pages=4 sum={:#x}",
                sum.value()
            )
        );
        let elf = bismuth_elf::Elf::parse(file).expect("valid");
        assert_eq!(long.lines[3], placed_line(file, &elf));
        assert!(long.lines[3].contains(" pages=4 "), "{}", long.lines[3]);
        assert_eq!(&long.lines[1], &normal.lines[1]);
    }

    #[test]
    fn the_started_scenarios_expect_the_jump_then_the_kernels_line() {
        let kernel = two_segment_kernel();
        let all = scenarios(&kernel).expect("scenarios");
        let jump = "BISMUTH KERNEL JUMP entry=0xffffffff80001040 stack=0xffffffff80000000 \
                    handoff=0xffffffff7ffe0000";
        // The kernel's own: its segments' pages, 16 of stack, 54 of tables.
        for (scenario, kernel) in [(&all[0], 2 + 16 + 54), (&all[8], 4 + 16 + 54)] {
            assert_eq!(scenario.lines.len(), 10, "{}", scenario.name);
            assert_eq!(scenario.lines[4], "Bismuth kernel addresses checked.");
            assert_eq!(
                scenario.lines[5],
                "BISMUTH FIRMWARE LEFT descriptors=[1..=512] usable=[usable:40000..=60000] bs=0"
            );
            assert_eq!(scenario.lines[6], jump);
            assert_eq!(scenario.lines[7], "BISMUTH KERNEL TABLES OK");
            assert_eq!(
                scenario.lines[8],
                format!(
                    "BISMUTH KERNEL MEMORY regions=[16..=256] usable=[=usable] kernel={kernel}"
                )
            );
            assert_eq!(scenario.lines[9], "BISMUTH KERNEL OK");
            assert_eq!(scenario.exit_code, 65);
        }
        // The kernel exits now, so nothing expects the boot stage's success.
        assert!(all.iter().all(|s| s.exit_code != QEMU_SUCCESS_EXIT));
        assert!(all.iter().all(|s| s.fault.is_none()));
        // Only the scenarios that reach the kernel leave the firmware.
        let leaving: Vec<_> = all
            .iter()
            .filter(|s| s.lines.iter().any(|l| l.contains("FIRMWARE")))
            .map(|s| s.name)
            .collect();
        assert_eq!(leaving, ["normal", "long-segment"]);
        let panic = panic_scenario(&kernel).expect("scenario");
        assert_eq!(panic.name, "kernel-panic");
        assert_eq!(panic.lines[..9], all[0].lines[..9]);
        assert_eq!(panic.lines[9], "BISMUTH KERNEL PANIC");
        assert_eq!(panic.absent, ["BISMUTH KERNEL OK"]);
        assert_eq!(panic.exit_code, 67);
        let damaged = damaged_handoff_scenario(&kernel, "damaged.efi".into()).expect("scenario");
        assert_eq!(damaged.name, "damaged-handoff");
        // The tables are loaded before the hand-off is read.
        assert_eq!(damaged.lines[..8], all[0].lines[..8]);
        assert_eq!(
            damaged.lines[8..],
            ["BISMUTH KERNEL HANDOFF ERROR: the marker is wrong"]
        );
        assert_eq!(damaged.absent, ["BISMUTH KERNEL OK"]);
        assert_eq!(damaged.boot.as_deref(), Some(Path::new("damaged.efi")));
        assert_eq!(damaged.exit_code, 67);
        // Only it boots another image.
        assert!(all.iter().all(|s| s.boot.is_none()) && panic.boot.is_none());
    }

    #[test]
    fn a_named_number_must_be_the_same_on_a_later_line() {
        let mut scenario = Scenario::new("named", None, "LEFT u=[u:1..=100]".to_string(), 65);
        scenario.lines.push("MEMORY u=[=u] k=3".to_string());
        for (serial, passes) in [
            (
                "BISMUTH BOOT OK
LEFT u=42
MEMORY u=42 k=3
",
                true,
            ),
            (
                "BISMUTH BOOT OK
LEFT u=42
MEMORY u=41 k=3
",
                false,
            ),
            (
                "BISMUTH BOOT OK
LEFT u=42
MEMORY u=420 k=3
",
                false,
            ),
            (
                "BISMUTH BOOT OK
LEFT u=42
MEMORY u= k=3
",
                false,
            ),
            (
                "BISMUTH BOOT OK
LEFT u=420
MEMORY u=420 k=3
",
                false,
            ),
            // A line that fails records nothing.
            (
                "BISMUTH BOOT OK
LEFT u=7 x
LEFT u=42
MEMORY u=7 k=3
",
                false,
            ),
            (
                "BISMUTH BOOT OK
MEMORY u=42 k=3
LEFT u=42
",
                false,
            ),
        ] {
            assert_eq!(
                boot_failure(&scenario, Some(65), serial).is_none(),
                passes,
                "{serial}"
            );
        }
        // An unknown name matches nothing.
        assert!(!super::line_matches("MEMORY u=[=u]", "MEMORY u=42"));
    }

    /// [`two_segment_kernel`] with a third, writable segment of three
    /// pages at `KERNEL_BASE + 0x2000`, 16 bytes of them from the file.
    fn three_segment_kernel() -> Vec<u8> {
        let mut file = two_segment_kernel();
        file[56..58].copy_from_slice(&3u16.to_le_bytes());
        let mut put =
            |at: usize, value: u64| file[at..at + 8].copy_from_slice(&value.to_le_bytes());
        put(176, 0x6_0000_0001); // PT_LOAD, then flags rw-
        put(184, 0x1000);
        put(192, KERNEL_BASE + 0x2000);
        put(208, 0x10);
        put(216, 0x3000);
        put(224, 0x1000);
        file
    }

    #[test]
    fn each_fault_scenario_expects_the_kernels_report_in_the_right_place() {
        let kernel = three_segment_kernel();
        let normal = &scenarios(&kernel).expect("scenarios")[0];
        let all: Vec<Scenario> = DELIBERATE_FAULTS
            .iter()
            .map(|fault| fault_scenario(fault, &kernel).expect("scenario"))
            .collect();
        let code = KERNEL_BASE + 0x1000..KERNEL_BASE + 0x2000;
        let read_only = KERNEL_BASE..KERNEL_BASE + 0x1000;
        let writable = KERNEL_BASE + 0x2000..KERNEL_BASE + 0x5000;
        let guard = STACK_GUARD..STACK_BOTTOM;
        let expected = [
            ("fault-divide", "vector=0 #DE error=0x0", Some(&code), None),
            (
                "fault-invalid-opcode",
                "vector=6 #UD error=0x0",
                Some(&code),
                None,
            ),
            (
                "fault-write-read-only",
                "vector=14 #PF error=0x3",
                Some(&code),
                Some(&read_only),
            ),
            (
                "fault-run-data",
                "vector=14 #PF error=0x11",
                Some(&writable),
                Some(&writable),
            ),
            (
                "fault-guard-page",
                "vector=14 #PF error=0x0",
                Some(&code),
                Some(&guard),
            ),
            ("fault-stack-overflow", "vector=8 #DF error=0x0", None, None),
            (
                "fault-last-vector",
                "vector=255 interrupt error=0x0",
                Some(&code),
                None,
            ),
        ];
        assert_eq!(all.len(), expected.len());
        for (scenario, (name, start, rip, cr2)) in all.iter().zip(expected) {
            assert_eq!(scenario.name, name);
            assert_eq!(scenario.exit_code, 67, "{name}");
            // The lines of a normal boot up to the kernel's `MEMORY` line.
            assert_eq!(scenario.lines[..], normal.lines[..9], "{name}");
            assert_eq!(scenario.absent, ["BISMUTH KERNEL OK"], "{name}");
            let fault = scenario.fault.as_ref().expect("a fault");
            assert_eq!(fault.start, format!("BISMUTH KERNEL FAULT {start}"));
            assert_eq!(fault.rip.as_ref(), rip, "{name}");
            assert_eq!(fault.cr2.as_ref(), cr2, "{name}");
            let rsp = if name == "fault-stack-overflow" {
                STACK_GUARD..STACK_BOTTOM + 0x1000
            } else {
                STACK_BOTTOM..STACK_TOP
            };
            assert_eq!(fault.rsp, rsp, "{name}");
        }
        // Without a writable segment, the scenario that runs one cannot be
        // made.
        let fault = &DELIBERATE_FAULTS[3];
        assert!(fault_scenario(fault, &two_segment_kernel()).is_err());
    }

    #[test]
    fn a_fault_report_must_match_in_every_field() {
        let fault = FaultLine {
            start: "BISMUTH KERNEL FAULT vector=14 #PF error=0x3".to_string(),
            rip: Some(0x1000..0x2000),
            rsp: 0x8000..0x9000,
            cr2: Some(0x3000..0x4000),
        };
        let good = "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x1abc rsp=0x8ff0 cr2=0x3001";
        assert_eq!(fault.mismatch(good), None);
        for (line, reason) in [
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x2 rip=0x1abc rsp=0x8ff0 cr2=0x3001",
                "does not start",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x31 rip=0x1abc rsp=0x8ff0 cr2=0x3001",
                "does not have the fields",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x2000 rsp=0x8ff0 cr2=0x3001",
                "rip=0x2000 is outside",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x1abc rsp=0x7ff0 cr2=0x3001",
                "rsp=0x7ff0 is outside",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x1abc rsp=0x8ff0 cr2=0x4000",
                "cr2=0x4000 is outside",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x1abc rsp=0x8ff0",
                "does not have the fields",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x1abc rsp=0x8ff0 cr2=0x3001 x",
                "does not have the fields",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x1abc rsp=0x8ff0 cr2=0xzz",
                "is not cr2=0x<hexadecimal>",
            ),
            (
                "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rsp=0x8ff0 rip=0x1abc cr2=0x3001",
                "is not rip=",
            ),
        ] {
            let found = fault.mismatch(line).expect("mismatch");
            assert!(found.contains(reason), "{line}: {found}");
        }
        // A double fault's instruction is not checked, and it has no `cr2`.
        let double = FaultLine {
            rip: None,
            cr2: None,
            ..fault
        };
        let line = "BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0x0 rsp=0x8ff0";
        assert_eq!(double.mismatch(line), None);
    }

    #[test]
    fn a_fault_scenario_fails_without_the_kernels_report() {
        let kernel = three_segment_kernel();
        let mut scenario = fault_scenario(&DELIBERATE_FAULTS[0], &kernel).expect("scenario");
        // Only the report is checked here.
        scenario.lines.truncate(1);
        let fault = scenario.fault.clone().expect("a fault");
        let report = format!(
            "{} rip={:#x} rsp={:#x}",
            fault.start,
            KERNEL_BASE + 0x1800,
            STACK_TOP - 0x100
        );
        let serial = format!("BISMUTH BOOT OK\r\n{report}\r\n");
        assert_eq!(boot_failure(&scenario, Some(67), &serial), None);
        let failure = boot_failure(&scenario, Some(67), "BISMUTH BOOT OK\r\n").expect("fails");
        assert!(failure.contains("did not contain a"), "{failure}");
        let serial = format!("BISMUTH BOOT OK\r\n{report}\r\nBISMUTH KERNEL OK\r\n");
        let failure = boot_failure(&scenario, Some(67), &serial).expect("fails");
        assert!(
            failure.contains("contained \"BISMUTH KERNEL OK\""),
            "{failure}"
        );
    }

    #[test]
    fn only_the_no_execute_scenario_changes_the_processor() {
        let kernel = two_segment_kernel();
        let all = scenarios(&kernel).expect("scenarios");
        let changed: Vec<_> = all.iter().filter(|s| s.cpu.is_some()).collect();
        assert_eq!(changed.len(), 1);
        let scenario = changed[0];
        assert_eq!(
            (scenario.name, scenario.cpu, scenario.exit_code),
            ("no-execute", Some("qemu64,-nx"), 35)
        );
        assert_eq!(scenario.kernel.as_deref(), Some(&kernel[..]));
        assert_eq!(
            scenario.lines.last().map(String::as_str),
            Some("BISMUTH KERNEL MAP ERROR: the processor cannot mark pages no-execute")
        );
        assert_eq!(scenario.absent, ["Bismuth kernel addresses checked."]);
    }

    #[test]
    fn the_top_page_scenario_moves_the_last_segment_there() {
        // The code shortened to half a page, so that on the top page it
        // ends below 2^64, which the ELF reader requires.
        let mut kernel = two_segment_kernel();
        kernel[152..168].copy_from_slice(&[0, 8, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0]);
        let all = scenarios(&kernel).expect("scenarios");
        let top = all.iter().find(|s| s.name == "top-page").expect("top-page");
        let file = top.kernel.as_deref().expect("has a file");
        let elf = bismuth_elf::Elf::parse(file).expect("the ELF reader accepts it");
        assert_eq!(
            bismuth_elf::place::place(&elf).map(|_| ()),
            Err(PlaceError::LastPage { segment: 1 })
        );
        assert_eq!(
            top.lines[1],
            "BISMUTH KERNEL PLACE ERROR: segment 1 uses the last page of the address range"
        );
        assert_eq!(top.exit_code, 35);
    }

    #[test]
    fn page_images_start_mid_page_and_are_padded_with_zeros() {
        let mut file = kernel_file(0b101, KERNEL_BASE + 0x40, 1);
        // gABI ch. 5, "Program Header": offset, address, file and memory
        // sizes at 8, 16, 32 and 40 in the header at 64.
        file[72..80].copy_from_slice(&0x810u64.to_le_bytes());
        file[80..88].copy_from_slice(&(KERNEL_BASE + 0x810).to_le_bytes());
        file[96..104].copy_from_slice(&4u64.to_le_bytes());
        file[104..112].copy_from_slice(&0x1000u64.to_le_bytes());
        file[0x810..0x814].copy_from_slice(b"BISM");
        let elf = bismuth_elf::Elf::parse(&file).expect("valid");
        let images = page_images(&file, &elf);
        assert_eq!(images.len(), 1);
        let image = &images[0];
        assert_eq!(image.len(), 0x2000);
        assert_eq!(&image[0x810..0x814], b"BISM");
        assert!(
            image[..0x810]
                .iter()
                .chain(&image[0x814..])
                .all(|b| *b == 0)
        );
    }

    /// What a fake step does when it runs.
    #[derive(Clone, Copy)]
    enum Fake {
        Pass,
        Note,
        Fail,
        NoQemu,
    }

    /// Runs `check`'s step runner on fake steps named `a`, `b`, ... that
    /// behave as `fakes` says. Returns the lines it reported, the names of
    /// the steps that ran, and the exit code.
    fn run_fakes(fakes: &[Fake], require_qemu: bool) -> (Vec<String>, Vec<&'static str>, u8) {
        const NAMES: [&str; 4] = ["a", "b", "c", "d"];
        let ran = RefCell::new(Vec::new());
        let steps: Vec<Step> = fakes
            .iter()
            .zip(NAMES)
            .map(|(&fake, name)| -> Step {
                let ran = &ran;
                let step = move || {
                    ran.borrow_mut().push(name);
                    match fake {
                        Fake::Pass => Ok(String::new()),
                        Fake::Note => Ok("3 things".to_string()),
                        Fake::Fail => Err(Stop::Failed(format!("{name} broke"))),
                        Fake::NoQemu => Err(Stop::QemuMissing("looked here".to_string())),
                    }
                };
                (name, Box::new(step))
            })
            .collect();
        let mut lines = Vec::new();
        let code = run_steps(steps, require_qemu, &mut |line| {
            lines.push(line.to_string())
        });
        (lines, ran.into_inner(), code)
    }

    #[test]
    fn a_check_where_every_step_passes_ends_with_check_passed() {
        let (lines, ran, code) = run_fakes(&[Fake::Pass, Fake::Note, Fake::Pass], false);
        assert_eq!(
            lines,
            [
                "check: a: ok",
                "check: b: ok, 3 things",
                "check: c: ok",
                "check passed"
            ]
        );
        assert_eq!(ran, ["a", "b", "c"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn a_check_stops_at_the_first_failing_step_and_names_it() {
        for (at, name) in ["a", "b", "c"].into_iter().enumerate() {
            let mut fakes = [Fake::Pass; 3];
            fakes[at] = Fake::Fail;
            let (lines, ran, code) = run_fakes(&fakes, false);
            let mut expected: Vec<String> = ran[..at]
                .iter()
                .map(|n| format!("check: {n}: ok"))
                .collect();
            expected.push(format!("check: {name}: FAILED: {name} broke"));
            expected.push(format!("check FAILED at {name}"));
            assert_eq!(lines, expected);
            // The steps after the failing one do not run.
            assert_eq!(ran, ["a", "b", "c"][..=at]);
            assert_eq!(code, 1);
        }
    }

    #[test]
    fn a_check_without_qemu_runs_the_rest_and_ends_not_run() {
        let (lines, ran, code) = run_fakes(&[Fake::Pass, Fake::NoQemu, Fake::Pass], false);
        assert_eq!(
            lines,
            [
                "check: a: ok",
                "check: b: NOT RUN: looked here",
                "check: c: ok",
                "b NOT RUN: QEMU is not installed"
            ]
        );
        assert_eq!(ran, ["a", "b", "c"]);
        assert_eq!(code, 3);
    }

    #[test]
    fn a_check_without_qemu_fails_when_qemu_is_required() {
        let (lines, ran, code) = run_fakes(&[Fake::Pass, Fake::NoQemu, Fake::Pass], true);
        assert_eq!(
            lines,
            [
                "check: a: ok",
                "check: b: FAILED: QEMU is not installed and BISMUTH_REQUIRE_QEMU is set: \
                 looked here",
                "check FAILED at b"
            ]
        );
        assert_eq!(ran, ["a", "b"]);
        assert_eq!(code, 1);
    }

    #[test]
    fn a_failure_wins_over_missing_qemu() {
        // A failure before the step that needs QEMU: that step never runs.
        let (lines, ran, code) = run_fakes(&[Fake::Fail, Fake::NoQemu], false);
        assert_eq!(lines, ["check: a: FAILED: a broke", "check FAILED at a"]);
        assert_eq!(ran, ["a"]);
        assert_eq!(code, 1);
        // A failure after it still ends the check as failed.
        let (lines, _, code) = run_fakes(&[Fake::NoQemu, Fake::Fail], false);
        assert_eq!(
            lines,
            [
                "check: a: NOT RUN: looked here",
                "check: b: FAILED: b broke",
                "check FAILED at b"
            ]
        );
        assert_eq!(code, 1);
    }

    /// `locate_qemu` with `variable` set or not and QEMU's usual install
    /// path present or not, where the `PATH` lookup answers `on_path`.
    fn locate(variable: Option<&str>, on_path: bool, usual_exists: bool) -> Result<PathBuf> {
        let usual = Path::new("C:/usual/qemu.exe");
        locate_qemu(
            variable.map(OsString::from),
            || on_path,
            &[(usual, usual_exists)],
        )
    }

    #[test]
    fn a_set_bismuth_qemu_is_used_as_given() {
        // A bare name or a path without `.exe` that QEMU starts from.
        for value in [
            "qemu-system-x86_64",
            "C:/Program Files/qemu/qemu-system-x86_64",
        ] {
            assert_eq!(locate(Some(value), false, false), Ok(PathBuf::from(value)));
        }
        // One QEMU cannot start from is still used, so starting it fails
        // with the reason instead of falling back or counting as not
        // installed, and `PATH` is not searched.
        let missing = "C:/no/such/qemu";
        let not_asked = || -> bool { panic!("PATH was searched") };
        assert_eq!(
            locate_qemu(Some(missing.into()), not_asked, &[]),
            Ok(PathBuf::from(missing))
        );
        assert_eq!(
            locate(Some(missing), true, true),
            Ok(PathBuf::from(missing))
        );
    }

    #[test]
    fn an_unset_bismuth_qemu_finds_qemu_on_path_then_in_its_usual_place() {
        assert_eq!(
            locate(None, true, true),
            Ok(PathBuf::from("qemu-system-x86_64"))
        );
        assert_eq!(
            locate(None, false, true),
            Ok(PathBuf::from("C:/usual/qemu.exe"))
        );
    }

    #[test]
    fn qemu_found_nowhere_says_where_it_looked() {
        assert_eq!(
            locate(None, false, false),
            Err(
                "BISMUTH_QEMU is not set, qemu-system-x86_64 is not on PATH, and \
                 C:/usual/qemu.exe does not exist"
                    .to_string()
            )
        );
    }

    #[test]
    fn finds_paths_in_code_spans() {
        let text = "The stage is `boot/src/main.rs` and the hook is `.githooks/commit-msg`.";
        assert_eq!(
            paths_in_markdown(text),
            ["boot/src/main.rs", ".githooks/commit-msg"]
        );
    }

    #[test]
    fn ignores_commands_and_plain_names() {
        let text = "Run `cargo xtask test`. The entry point is `efi_main`.";
        assert!(paths_in_markdown(text).is_empty());
    }

    #[test]
    fn ignores_fenced_code_blocks() {
        let text = "```sh\ncat `not/a/path`\n```\nBut `docs/boot.md` counts.";
        assert_eq!(paths_in_markdown(text), ["docs/boot.md"]);
    }

    #[test]
    fn drops_a_trailing_slash_from_folders() {
        assert_eq!(paths_in_markdown("See `boot/src/`."), ["boot/src"]);
    }

    #[test]
    fn ignores_text_outside_code_spans() {
        assert!(paths_in_markdown("A path like boot/src/main.rs in prose.").is_empty());
    }
}
