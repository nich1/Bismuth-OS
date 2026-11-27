//! Commands that talk to the code host, GitHub, through its command-line tool
//! `gh`: reading, commenting on and filing issues, and opening, editing,
//! marking ready and waiting on pull requests.
//!
//! Deciding is kept apart from doing: the argument lists, the choice of
//! credential and the reading of check results are plain functions with unit
//! tests, and only the thin code at the bottom of the file starts processes.
//!
//! The credential git holds is handed to `gh` only through the `GH_TOKEN`
//! environment variable of that one process. It is never printed, put in an
//! argument list, written to a file or included in an error message, so a
//! [`Command`] holding it must never be formatted with `{:?}`, which shows
//! its environment.

use std::env;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::Result;

/// Words that credit a tool. Kept equal to `tools/check-attribution.sh` by a
/// unit test.
const CREDIT_WORDS: &[&str] = &["claude", "anthropic", "chatgpt", "openai"];
/// Names removed before the search, so the `.claude` folder and `CLAUDE.md`
/// can be mentioned. Kept equal to the script by the same test.
const ALLOWED_NAMES: &[&str] = &[".claude", "claude.md"];

/// How long `pr-wait` waits for the first check to be created.
const FIRST_CHECK_LIMIT: Duration = Duration::from_secs(120);
/// How long `pr-wait` waits in all.
const WAIT_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How often `pr-wait` asks for the checks' state.
const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// How much of a failing run's log `pr-wait` prints.
const LOG_TAIL_LINES: usize = 60;

/// One line per check from `gh pr checks`: bucket, name and link.
const CHECKS_JQ: &str = ".[] | [.bucket, .name, .link] | @tsv";
/// The issue's title and body, then each comment with its author and date.
const ISSUE_JQ: &str = r#".title + "\n\n" + .body + ([.comments[] | "\n\n--- comment by " + .author.login + ", " + .createdAt + "\n\n" + .body] | join(""))"#;

/// Returns true if the text credits a tool, by the rule in
/// `tools/check-attribution.sh`: lower-case it, remove the allowed names,
/// then look for any of the words.
fn credits_a_tool(text: &str) -> bool {
    let mut text = text.to_ascii_lowercase();
    for name in ALLOWED_NAMES {
        text = text.replace(name, "");
    }
    CREDIT_WORDS.iter().any(|word| text.contains(word))
}

/// Fails if `text` credits a tool. `what` names it in the error.
fn check_credit(what: &str, text: &str) -> Result<()> {
    if credits_a_tool(text) {
        Err(format!(
            "the {what} credits an AI tool; remove the credit line before sending it"
        ))
    } else {
        Ok(())
    }
}

/// A credential that must not be shown. It has no `Display`, and `Debug`
/// hides the value.
struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

/// How `gh` is signed in.
#[derive(Debug)]
enum Credential {
    /// `gh` has its own sign-in; nothing is added.
    GhOwn,
    /// The token git holds, given to `gh` as `GH_TOKEN`.
    Token(Secret),
}

/// Takes the `password=` value from what `git credential fill` prints.
fn password_from(fill_output: &str) -> Option<Secret> {
    fill_output
        .lines()
        .find_map(|line| line.strip_prefix("password="))
        .filter(|password| !password.is_empty())
        .map(|password| Secret(password.to_string()))
}

/// Uses `gh`'s own sign-in if it has one, otherwise the token git holds.
/// `from_git` is only called when `gh` is not signed in.
fn choose_credential(
    gh_signed_in: bool,
    from_git: impl FnOnce() -> Option<Secret>,
) -> Result<Credential> {
    if gh_signed_in {
        return Ok(Credential::GhOwn);
    }
    from_git().map(Credential::Token).ok_or_else(|| {
        "gh is not signed in and git holds no credential for github.com. Sign in \
         yourself with `gh auth login`, or sign git in to github.com (for example \
         by pushing once from a terminal), then run this again"
            .into()
    })
}

fn issue_read_args(number: &str) -> Vec<String> {
    let json = ["--json", "title,body,comments", "--jq", ISSUE_JQ];
    let mut args = strings(&["issue", "view", number]);
    args.extend(strings(&json));
    args
}

fn issue_comment_args(number: &str, body_file: &str) -> Vec<String> {
    strings(&["issue", "comment", number, "--body-file", body_file])
}

fn issue_new_args(title: &str, body_file: &str, label: Option<&str>) -> Vec<String> {
    let mut args = strings(&[
        "issue",
        "create",
        "--title",
        title,
        "--body-file",
        body_file,
    ]);
    if let Some(label) = label {
        args.extend(strings(&["--label", label]));
    }
    args
}

/// The `git push` arguments for `branch`. Refuses `main` and a detached head.
fn push_args(branch: &str) -> Result<Vec<String>> {
    match branch {
        "" => Err("no branch is checked out; check out the task's branch first".into()),
        "main" => Err("refusing to push main; work on a `type/short-description` branch".into()),
        _ => Ok(strings(&["push", "-u", "origin", branch])),
    }
}

fn pr_create_args(branch: &str, title: &str, body_file: &str, draft: bool) -> Vec<String> {
    let mut args = strings(&["pr", "create", "--base", "main", "--head", branch]);
    args.extend(strings(&["--title", title, "--body-file", body_file]));
    if draft {
        args.push("--draft".into());
    }
    args
}

fn pr_edit_args(body_file: &str) -> Vec<String> {
    strings(&["pr", "edit", "--body-file", body_file])
}

fn pr_ready_args() -> Vec<String> {
    strings(&["pr", "ready"])
}

fn pr_checks_args(number: Option<&str>) -> Vec<String> {
    let mut args = strings(&["pr", "checks"]);
    args.extend(number.map(str::to_string));
    args.extend(strings(&["--json", "name,bucket,link", "--jq", CHECKS_JQ]));
    args
}

/// One check on a pull request, as `gh pr checks` reports it.
#[derive(Debug, PartialEq)]
struct Check {
    /// `pass`, `fail`, `pending`, `skipping` or `cancel`.
    bucket: String,
    name: String,
    link: String,
}

/// Reads the tab-separated lines [`CHECKS_JQ`] produces. A missing field is
/// left empty, and an empty bucket counts as a failure.
fn parse_checks(text: &str) -> Vec<Check> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.trim_end_matches('\r').splitn(3, '\t');
            let mut next = || fields.next().unwrap_or("").to_string();
            Check {
                bucket: next(),
                name: next(),
                link: next(),
            }
        })
        .collect()
}

/// What `pr-wait` does after reading the checks.
#[derive(Debug, PartialEq)]
enum Step<'a> {
    /// Ask again after [`POLL_INTERVAL`].
    Poll,
    /// Every check finished and none failed.
    Passed,
    /// Every check finished and these failed.
    Failed(Vec<&'a Check>),
    /// Waited too long; the reason says for what.
    GiveUp(String),
}

/// Decides what to do given the checks so far and the time spent waiting.
///
/// No checks yet means they have not been created, which is neither a pass
/// nor a failure. A bucket other than `pass`, `skipping` or `pending` is a
/// failure.
fn decide(checks: &[Check], elapsed: Duration) -> Step<'_> {
    let (limit, waiting_for) = if checks.is_empty() {
        (FIRST_CHECK_LIMIT, "the first check to appear")
    } else if checks.iter().any(|check| check.bucket == "pending") {
        (WAIT_LIMIT, "the checks to finish")
    } else {
        let failed: Vec<&Check> = checks
            .iter()
            .filter(|check| !matches!(check.bucket.as_str(), "pass" | "skipping"))
            .collect();
        return if failed.is_empty() {
            Step::Passed
        } else {
            Step::Failed(failed)
        };
    };
    if elapsed >= limit {
        Step::GiveUp(format!(
            "gave up after {} seconds waiting for {waiting_for}",
            limit.as_secs()
        ))
    } else {
        Step::Poll
    }
}

/// The workflow run a check's link points at:
/// `https://github.com/<owner>/<repo>/actions/runs/<run>/job/<job>`.
fn run_id(link: &str) -> Option<&str> {
    let (_, rest) = link.split_once("/actions/runs/")?;
    let run = rest.split('/').next()?;
    (!run.is_empty() && run.bytes().all(|byte| byte.is_ascii_digit())).then_some(run)
}

/// The last `lines` lines of `text`.
fn tail(text: &str, lines: usize) -> &str {
    let body = text.strip_suffix('\n').unwrap_or(text);
    match body.rmatch_indices('\n').nth(lines.saturating_sub(1)) {
        Some((index, _)) => &text[index + 1..],
        _ => text,
    }
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

// Everything below starts processes.

/// Finds the `gh` program: `BISMUTH_GH`, then `PATH`, then the default
/// Windows install folder.
fn find_gh() -> Result<PathBuf> {
    if let Some(path) = env::var_os("BISMUTH_GH") {
        return Ok(path.into());
    }
    let on_path = Command::new("gh")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if on_path {
        return Ok("gh".into());
    }
    let windows_default = Path::new(r"C:\Program Files\GitHub CLI\gh.exe");
    if windows_default.is_file() {
        return Ok(windows_default.to_path_buf());
    }
    Err(
        "could not find gh, GitHub's command-line tool. Install it, or set \
         BISMUTH_GH to its path"
            .into(),
    )
}

/// Asks git for the github.com credential it holds, without letting it
/// prompt. Its output is never shown.
fn token_from_git() -> Option<Secret> {
    let mut child = Command::new("git")
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let written = child
        .stdin
        .take()?
        .write_all(b"protocol=https\nhost=github.com\n\n");
    let output = child.wait_with_output().ok()?;
    if written.is_err() || !output.status.success() {
        return None;
    }
    password_from(&String::from_utf8_lossy(&output.stdout))
}

/// `gh`, signed in.
struct Gh {
    program: PathBuf,
    credential: Credential,
}

impl Gh {
    fn new() -> Result<Gh> {
        let program = find_gh()?;
        let signed_in = Command::new(&program)
            .args(["auth", "status"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        let credential = choose_credential(signed_in, token_from_git)?;
        Ok(Gh {
            program,
            credential,
        })
    }

    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.program);
        command.current_dir(crate::root()).args(args);
        if let Credential::Token(Secret(token)) = &self.credential {
            command.env("GH_TOKEN", token);
        }
        command
    }

    /// Runs `gh` with its output shown, failing if it fails. The error names
    /// only the subcommand.
    fn run(&self, args: &[String]) -> Result<()> {
        let what = format!("gh {}", args[..2].join(" "));
        crate::run_checked(&mut self.command(args), &what)
    }

    /// Runs `gh` and captures its output.
    fn output(&self, args: &[String]) -> Result<Output> {
        self.command(args)
            .output()
            .map_err(|error| format!("could not start gh: {error}"))
    }
}

/// Reads a body file and fails if it credits a tool.
fn checked_body(path: &str) -> Result<()> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("could not read {path}: {error}"))?;
    check_credit("body", &text)
}

/// The branch checked out now.
fn current_branch() -> Result<String> {
    let output = Command::new("git")
        .current_dir(crate::root())
        .args(["branch", "--show-current"])
        .output()
        .map_err(|error| format!("could not start git: {error}"))?;
    if !output.status.success() {
        return Err("git branch --show-current failed".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn issue_read(number: &str) -> Result<()> {
    Gh::new()?.run(&issue_read_args(number))
}

pub fn issue_comment(number: &str, body_file: &str) -> Result<()> {
    checked_body(body_file)?;
    Gh::new()?.run(&issue_comment_args(number, body_file))
}

pub fn issue_new(title: &str, body_file: &str, label: Option<&str>) -> Result<()> {
    check_credit("title", title)?;
    checked_body(body_file)?;
    Gh::new()?.run(&issue_new_args(title, body_file, label))
}

pub fn pr_open(title: &str, body_file: &str, draft: bool) -> Result<()> {
    check_credit("title", title)?;
    checked_body(body_file)?;
    let branch = current_branch()?;
    let push = push_args(&branch)?;
    let gh = Gh::new()?;
    crate::run_checked(
        Command::new("git").current_dir(crate::root()).args(&push),
        "git push",
    )?;
    gh.run(&pr_create_args(&branch, title, body_file, draft))
}

pub fn pr_edit(body_file: &str) -> Result<()> {
    checked_body(body_file)?;
    Gh::new()?.run(&pr_edit_args(body_file))
}

pub fn pr_ready() -> Result<()> {
    Gh::new()?.run(&pr_ready_args())
}

/// Blocks until every check on the pull request has finished, prints one
/// line per check, and fails with the end of the failing runs' logs if any
/// failed.
pub fn pr_wait(number: Option<&str>) -> Result<()> {
    let gh = Gh::new()?;
    let start = Instant::now();
    eprintln!("waiting for the pull request's checks to finish");
    loop {
        let output = gh.output(&pr_checks_args(number))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // With checks pending, gh exits with 8; with none yet, with 1 and
        // "no checks reported".
        let checks = if !stdout.trim().is_empty() {
            parse_checks(&stdout)
        } else if output.status.success()
            || stderr.to_ascii_lowercase().contains("no checks reported")
        {
            Vec::new()
        } else {
            return Err(format!("gh pr checks failed: {}", stderr.trim()));
        };
        match decide(&checks, start.elapsed()) {
            Step::Poll => thread::sleep(POLL_INTERVAL),
            Step::GiveUp(reason) => return Err(reason),
            Step::Passed => {
                print_checks(&checks);
                return Ok(());
            }
            Step::Failed(failed) => {
                print_checks(&checks);
                let mut runs: Vec<&str> = failed.iter().filter_map(|c| run_id(&c.link)).collect();
                runs.dedup();
                for run in runs {
                    let log = gh.output(&strings(&["run", "view", run, "--log-failed"]))?;
                    println!("--- end of the failing log of run {run} ---");
                    print!(
                        "{}",
                        tail(&String::from_utf8_lossy(&log.stdout), LOG_TAIL_LINES)
                    );
                }
                return Err(format!(
                    "{} of {} checks failed",
                    failed.len(),
                    checks.len()
                ));
            }
        }
    }
}

fn print_checks(checks: &[Check]) {
    for check in checks {
        println!("{}\t{}\t{}", check.bucket, check.name, check.link);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quoted pattern after `grep -Eq` in the script, split on `|`.
    fn script_words(script: &str) -> Vec<String> {
        let (_, rest) = script
            .split_once("grep -Eq '")
            .expect("the script has a grep -Eq line");
        let pattern = rest.split('\'').next().expect("the pattern is quoted");
        pattern.split('|').map(str::to_string).collect()
    }

    /// The names the script's `sed -e 's/NAME//g'` expressions remove.
    fn script_allowed(script: &str) -> Vec<String> {
        script
            .split("-e 's/")
            .skip(1)
            .map(|rest| rest.split("//g").next().unwrap().replace("\\.", "."))
            .collect()
    }

    #[test]
    fn credit_rule_matches_the_script() {
        let script = include_str!("../../tools/check-attribution.sh");
        assert_eq!(script_words(script), CREDIT_WORDS);
        assert_eq!(script_allowed(script), ALLOWED_NAMES);
    }

    #[test]
    fn credit_check_finds_credits_in_any_case() {
        assert!(credits_a_tool("Co-authored-by: Claude <x@example.com>"));
        assert!(credits_a_tool("made with ChatGPT"));
        assert!(credits_a_tool("line one\nsee ANTHROPIC docs"));
        assert!(!credits_a_tool("feat: add serial driver"));
    }

    #[test]
    fn credit_check_allows_the_folder_and_file_names() {
        assert!(!credits_a_tool("edit .claude/skills and CLAUDE.md"));
        assert!(credits_a_tool("edit .claude/skills, thanks claude"));
    }

    #[test]
    fn password_line_is_taken_from_git_output() {
        let output = "protocol=https\nhost=github.com\nusername=me\npassword=abc123\n";
        assert_eq!(password_from(output).map(|s| s.0), Some("abc123".into()));
        assert!(password_from("protocol=https\nhost=github.com\n").is_none());
        assert!(password_from("password=\n").is_none());
    }

    #[test]
    fn secret_is_hidden_when_formatted() {
        let shown = format!("{:?}", Credential::Token(Secret("abc123".into())));
        assert!(!shown.contains("abc123"), "{shown}");
    }

    #[test]
    fn gh_sign_in_is_used_without_asking_git() {
        let chosen = choose_credential(true, || panic!("git must not be asked"));
        assert!(matches!(chosen, Ok(Credential::GhOwn)));
    }

    #[test]
    fn git_token_is_used_when_gh_is_not_signed_in() {
        let chosen = choose_credential(false, || Some(Secret("abc123".into())));
        assert!(matches!(chosen, Ok(Credential::Token(Secret(t))) if t == "abc123"));
    }

    #[test]
    fn no_credential_says_what_to_do() {
        let error = choose_credential(false, || None).unwrap_err();
        assert!(error.contains("gh auth login"), "{error}");
    }

    #[test]
    fn issue_args() {
        assert_eq!(
            issue_read_args("7"),
            [
                "issue",
                "view",
                "7",
                "--json",
                "title,body,comments",
                "--jq",
                ISSUE_JQ
            ]
        );
        assert_eq!(
            issue_comment_args("7", "plan.md"),
            ["issue", "comment", "7", "--body-file", "plan.md"]
        );
        assert_eq!(
            issue_new_args("chore: x", "b.md", None),
            [
                "issue",
                "create",
                "--title",
                "chore: x",
                "--body-file",
                "b.md"
            ]
        );
        assert_eq!(
            issue_new_args("chore: x", "b.md", Some("papercut")),
            [
                "issue",
                "create",
                "--title",
                "chore: x",
                "--body-file",
                "b.md",
                "--label",
                "papercut"
            ]
        );
    }

    #[test]
    fn push_never_targets_main_or_forces() {
        assert_eq!(
            push_args("feat/x").unwrap(),
            ["push", "-u", "origin", "feat/x"]
        );
        assert!(push_args("main").is_err());
        assert!(push_args("").is_err());
    }

    #[test]
    fn pr_args() {
        assert_eq!(
            pr_create_args("feat/x", "feat: x", "b.md", false),
            [
                "pr",
                "create",
                "--base",
                "main",
                "--head",
                "feat/x",
                "--title",
                "feat: x",
                "--body-file",
                "b.md"
            ]
        );
        assert_eq!(
            pr_create_args("feat/x", "feat: x", "b.md", true)
                .last()
                .unwrap(),
            "--draft"
        );
        assert_eq!(pr_edit_args("b.md"), ["pr", "edit", "--body-file", "b.md"]);
        assert_eq!(pr_ready_args(), ["pr", "ready"]);
        assert_eq!(
            pr_checks_args(None),
            [
                "pr",
                "checks",
                "--json",
                "name,bucket,link",
                "--jq",
                CHECKS_JQ
            ]
        );
        assert_eq!(pr_checks_args(Some("17"))[2], "17");
    }

    fn check(bucket: &str, name: &str) -> Check {
        Check {
            bucket: bucket.into(),
            name: name.into(),
            link: format!("https://github.com/o/r/actions/runs/9/job/{name}"),
        }
    }

    #[test]
    fn check_lines_are_read() {
        let text = "pass\tBoot\thttps://a/runs/1/job/2\nfail\tText\thttps://a/runs/3/job/4\n\n";
        let checks = parse_checks(text);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[1].bucket, "fail");
        assert_eq!(checks[1].name, "Text");
        assert_eq!(checks[1].link, "https://a/runs/3/job/4");
    }

    #[test]
    fn no_checks_yet_is_neither_pass_nor_failure() {
        assert_eq!(decide(&[], Duration::from_secs(10)), Step::Poll);
        assert!(matches!(
            decide(&[], FIRST_CHECK_LIMIT),
            Step::GiveUp(reason) if reason.contains("first check")
        ));
    }

    #[test]
    fn pending_checks_are_waited_for_up_to_the_limit() {
        let checks = [check("pass", "a"), check("pending", "b")];
        assert_eq!(decide(&checks, Duration::from_secs(600)), Step::Poll);
        assert!(matches!(decide(&checks, WAIT_LIMIT), Step::GiveUp(_)));
    }

    #[test]
    fn finished_checks_give_a_verdict() {
        let passed = [check("pass", "a"), check("skipping", "b")];
        assert_eq!(decide(&passed, Duration::ZERO), Step::Passed);
        let failed = [check("pass", "a"), check("fail", "b"), check("cancel", "c")];
        assert_eq!(
            decide(&failed, Duration::ZERO),
            Step::Failed(vec![&failed[1], &failed[2]])
        );
    }

    #[test]
    fn run_id_comes_from_the_link() {
        let link = "https://github.com/o/r/actions/runs/46059099479/job/140708721738";
        assert_eq!(run_id(link), Some("46059099479"));
        assert_eq!(run_id("https://example.com/status"), None);
        assert_eq!(
            run_id("https://github.com/o/r/actions/runs/latest/job/1"),
            None
        );
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail("a\nb\n", 5), "a\nb\n");
    }
}
