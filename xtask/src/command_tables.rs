//! Keeps the command tables in `AGENTS.md` and `README.md` equal to the
//! commands the usage text in `main.rs` lists, so that neither goes stale
//! without a test failing. The usage text is the list; the tables follow it.

/// The documents with a command table, read when the tests are built.
const DOCUMENTS: &[(&str, &str)] = &[
    ("AGENTS.md", include_str!("../../AGENTS.md")),
    ("README.md", include_str!("../../README.md")),
];

/// The commands the usage text lists: the first word of each line indented
/// by exactly two spaces. Lines indented further continue a description.
fn usage_commands(usage: &str) -> Vec<&str> {
    usage
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .filter_map(|line| line.split_whitespace().next())
        .collect()
}

/// The usage text's lines that [`usage_commands`] would skip although they
/// could be a command: every line must be blank, a heading (unindented, and
/// the first line or ending in `:`), a command (two spaces) or a description
/// (ten spaces, under its command's name). Any other indentation, or a tab,
/// is reported.
fn usage_layout_problems(usage: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, line) in usage.lines().enumerate() {
        let text = line.trim_start();
        let indent = &line[..line.len() - text.len()];
        let problem = if indent.contains(|c| c != ' ') {
            "is not indented with spaces only".to_string()
        } else if text.is_empty() || matches!(indent.len(), 2 | 10) {
            continue;
        } else if indent.is_empty() {
            if index == 0 || text.ends_with(':') {
                continue;
            }
            "is not a heading ending in `:`".to_string()
        } else {
            format!("is indented by {} spaces", indent.len())
        };
        problems.push(format!("line {} {problem}: {line:?}", index + 1));
    }
    problems
}

/// The commands `main()` accepts: the first word of each pattern of its
/// `match args[..]`, in the layout `cargo fmt` gives it, with an arm's
/// pattern starting `["` or on its own line after `[`, eight spaces in.
fn dispatched_commands(source: &str) -> Vec<&str> {
    let (_, arms) = source
        .split_once("let result = match args[..] {\n")
        .expect("main() matches on args[..]");
    let arms = arms.split("\n        _ =>").next().unwrap_or("");
    let mut lines = arms.lines();
    let mut commands = Vec::new();
    while let Some(line) = lines.next() {
        let pattern = match line.strip_prefix("        [") {
            Some("") => lines.next().unwrap_or("").trim_start(),
            Some(pattern) => pattern,
            None => continue,
        };
        commands.extend(pattern.split('"').nth(1));
    }
    commands
}

/// The commands a document's command table names: the table whose header
/// starts `| Command |`, and in the first cell of each row every code span,
/// without `cargo xtask ` and without anything after the command's name.
fn table_commands(document: &str) -> Vec<&str> {
    let mut commands = Vec::new();
    let mut in_table = false;
    for line in document.lines() {
        if line.starts_with("| Command |") {
            in_table = true;
        } else if !line.starts_with('|') {
            in_table = false;
        } else if in_table {
            let first_cell = line.split('|').nth(1).unwrap_or("");
            // Splitting on backticks puts the code spans at the odd positions.
            for span in first_cell.split('`').skip(1).step_by(2) {
                let command = span.strip_prefix("cargo xtask ").unwrap_or(span);
                commands.extend(command.split_whitespace().next());
            }
        }
    }
    commands
}

/// What differs between the usage text and a document's command table.
fn table_problems(usage: &str, document: &str) -> Vec<String> {
    let listed = usage_commands(usage);
    let tabled = table_commands(document);
    let missing = listed.iter().filter(|command| !tabled.contains(command));
    let extra = tabled.iter().filter(|command| !listed.contains(command));
    missing
        .map(|command| format!("`{command}` is in the usage text but not in the command table"))
        .chain(extra.map(|command| {
            format!("`{command}` is in the command table but not in the usage text")
        }))
        .collect()
}

#[test]
fn every_command_table_matches_the_usage_text() {
    let mut problems = Vec::new();
    for (name, text) in DOCUMENTS {
        for problem in table_problems(crate::USAGE, text) {
            problems.push(format!("{name}: {problem}"));
        }
    }
    assert!(
        problems.is_empty(),
        "the command tables do not match the usage text in xtask/src/main.rs:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn the_usage_text_is_read_as_its_commands() {
    let usage = "usage: cargo xtask <task>\n\ntasks:\n  build   build it\n  \
                 snapshot [--update]\n          build, then print\n\
                 code host tasks:\n  pr-wait [<number>]\n";
    assert_eq!(usage_commands(usage), ["build", "snapshot", "pr-wait"]);
    let real = usage_commands(crate::USAGE);
    assert!(
        real.contains(&"build") && real.contains(&"issue-list"),
        "{real:?}"
    );
}

#[test]
fn the_usage_text_has_only_headings_commands_and_descriptions() {
    assert_eq!(usage_layout_problems(crate::USAGE), Vec::<String>::new());
    let usage = "usage: cargo xtask <task>\n\ntasks:\n  build   x\n          more of x\n\
                 \x20  pr-frob\n\tpr-tab\npr-flush\n        short continuation\n";
    assert_eq!(
        usage_layout_problems(usage),
        [
            "line 6 is indented by 3 spaces: \"   pr-frob\"",
            "line 7 is not indented with spaces only: \"\\tpr-tab\"",
            "line 8 is not a heading ending in `:`: \"pr-flush\"",
            "line 9 is indented by 8 spaces: \"        short continuation\"",
        ]
    );
}

#[test]
fn the_usage_text_lists_the_commands_main_accepts() {
    let mut listed = usage_commands(crate::USAGE);
    let mut accepted = dispatched_commands(include_str!("main.rs"));
    listed.sort_unstable();
    accepted.sort_unstable();
    accepted.dedup();
    assert_eq!(listed, accepted, "usage text, then main()");
}

#[test]
fn the_dispatch_is_read_as_its_commands() {
    let source = "fn x() {}\nfn main() {\n    let result = match args[..] {\n        \
                  [\"build\"] => build(false),\n        [\"snapshot\", \"--update\"] => \
                  snapshot(true),\n        [\n            \"issue-new\",\n            \
                  \"--title\",\n        ] => x(),\n        _ => {\n        [\"after\"]\n";
    assert_eq!(
        dispatched_commands(source),
        ["build", "snapshot", "issue-new"]
    );
}

#[test]
fn the_command_table_is_read_as_its_commands() {
    let document = "| Part | Where |\n|---|---|\n| Boot | `boot/x` |\n\n\
                    | Command | What it does |\n|---|---|\n\
                    | `cargo xtask snapshot` | prints; `--update` stores |\n\
                    | `cargo xtask pr-open`, `pr-edit` | open, edit |\n\nAfter `cargo xtask run`.\n\n\
                    | Part | Where |\n|---|---|\n| `kernel` | `kernel/x` |\n";
    assert_eq!(table_commands(document), ["snapshot", "pr-open", "pr-edit"]);
}

#[test]
fn a_missing_or_extra_command_is_reported() {
    let usage = "tasks:\n  build   x\n  run     y\n";
    let document = "| Command | What it does |\n|---|---|\n| `cargo xtask build`, `test` | z |\n";
    assert_eq!(
        table_problems(usage, document),
        [
            "`run` is in the usage text but not in the command table",
            "`test` is in the command table but not in the usage text"
        ]
    );
    assert_eq!(
        table_problems(usage, "no table here\n"),
        [
            "`build` is in the usage text but not in the command table",
            "`run` is in the usage text but not in the command table"
        ]
    );
}
