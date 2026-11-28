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
