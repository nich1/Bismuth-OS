//! Commands that talk to the code host, GitHub, through its command-line tool
//! `gh`: listing, reading, commenting on and filing issues, and opening, editing,
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
/// One line per issue: number and title. `@tsv` writes a tab, newline,
/// carriage return or backslash in the title as `\t`, `\n`, `\r` or `\\`.
const LIST_JQ: &str = ".[] | [.number, .title] | @tsv";
/// More issues than the repository will have open; `gh` stops at 30 without it.
const LIST_LIMIT: &str = "1000";

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

/// The label is joined to its flag, so a label such as `--web` stays a label.
fn issue_list_args(label: Option<&str>) -> Vec<String> {
    let mut args = strings(&["issue", "list", "--state", "open", "--limit", LIST_LIMIT]);
    args.extend(strings(&["--json", "number,title", "--jq", LIST_JQ]));
    args.extend(label.map(|label| format!("--label={label}")));
    args
}

fn label_list_args() -> Vec<String> {
    let json = ["--json", "name", "--jq", ".[].name"];
    let mut args = strings(&["label", "list", "--limit", LIST_LIMIT]);
    args.extend(strings(&json));
    args
}

/// What one run of `gh` printed, and whether it succeeded.
struct Reply {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Reads the lines [`LIST_JQ`] produces as `<number>\t<title>`, escaping any
/// control character `@tsv` left in the title so it cannot end the line,
/// move the cursor or clear the screen.
fn issue_lines(text: &str) -> Result<Vec<String>> {
    let lines = text.lines().filter(|line| !line.trim().is_empty());
    lines
        .map(|line| {
            let line = line.trim_end_matches('\r');
            match line.split_once('\t') {
                Some((number, title)) if crate::is_number(number) => {
                    let title: String = title
                        .chars()
                        .map(|c| {
                            if c.is_control() {
                                c.escape_unicode().to_string()
                            } else {
                                c.to_string()
                            }
                        })
                        .collect();
                    Ok(format!("{number}\t{title}"))
                }
                _ => Err(format!(
                    "gh issue list printed an unexpected line: {line:?}"
                )),
            }
        })
        .collect()
}

/// The open issues, with `label` if given, one line each. `gh` finds no
/// issues for a label that does not exist, so an empty list for a label is
/// checked against the repository's labels, making a misspelt label an error.
fn list_issues(
    label: Option<&str>,
    mut gh: impl FnMut(&[String]) -> Result<Reply>,
) -> Result<Vec<String>> {
    let reply = gh(&issue_list_args(label))?;
    if !reply.success {
        return Err(format!("gh issue list failed: {}", reply.stderr.trim()));
    }
    let issues = issue_lines(&reply.stdout)?;
    if let (true, Some(label)) = (issues.is_empty(), label) {
        let labels = gh(&label_list_args())?;
        if !labels.success {
            return Err(format!("gh label list failed: {}", labels.stderr.trim()));
        }
        // GitHub matches labels without regard to case.
        let mut names = labels
            .stdout
            .lines()
            .map(|name| name.trim_end_matches('\r'));
        if !names.any(|name| name.eq_ignore_ascii_case(label)) {
            return Err(format!("the repository has no label named {label:?}"));
        }
    }
    Ok(issues)
}

// The builders for commands that send text check it first, so a test can
// see that nothing reaches gh unchecked.

fn issue_comment_args(number: &str, body_file: &str) -> Result<Vec<String>> {
    let body = checked_body(body_file)?;
    Ok(strings(&["issue", "comment", number, "--body-file", &body]))
}

fn issue_new_args(title: &str, body_file: &str, label: Option<&str>) -> Result<Vec<String>> {
    check_credit("title", title)?;
    let body = checked_body(body_file)?;
    let mut args = strings(&["issue", "create", "--title", title, "--body-file", &body]);
    if let Some(label) = label {
        args.extend(strings(&["--label", label]));
    }
    Ok(args)
}

/// The `git push` arguments for `branch`. Refuses `main` and a detached head.
/// The full refspec means a name such as `+x` cannot force a push, and a
/// configured `remote.origin.push` cannot send the branch elsewhere.
fn push_args(branch: &str) -> Result<Vec<String>> {
    match branch {
        "" => Err("no branch is checked out; check out the task's branch first".into()),
        "main" => Err("refusing to push main; work on a `type/short-description` branch".into()),
        _ => {
            let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
            Ok(strings(&["push", "-u", "origin", &refspec]))
        }
    }
}

fn pr_create_args(branch: &str, title: &str, body_file: &str, draft: bool) -> Result<Vec<String>> {
    check_credit("title", title)?;
    let body = checked_body(body_file)?;
    let mut args = strings(&["pr", "create", "--base", "main", "--head", branch]);
    args.extend(strings(&["--title", title, "--body-file", &body]));
    if draft {
        args.push("--draft".into());
    }
    Ok(args)
}

/// Prints the number of the current branch's pull request.
fn pr_number_args() -> Vec<String> {
    strings(&["pr", "view", "--json", "number", "--jq", ".number"])
}

/// Replaces the description through the REST API. `gh pr edit` also reads
/// organisation fields, which git's token may not be allowed to. With `-F`,
/// a value read from `@file` is always sent as a string; gh fills in
/// `{owner}/{repo}` from the repository it runs in. Prints only the address.
fn pr_edit_args(number: &str, body_file: &str) -> Result<Vec<String>> {
    let field = format!("body=@{}", checked_body(body_file)?);
    let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}");
    let method = ["api", "--method", "PATCH"];
    let mut args = strings(&method);
    args.extend(strings(&[&endpoint, "-F", &field, "--jq", ".html_url"]));
    Ok(args)
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

/// Reads a body file and fails if it credits a tool. Returns the path to
/// give gh: made absolute once and used for both the reading and gh, so the
/// file checked is the file sent although gh runs from another folder.
fn checked_body(path: &str) -> Result<String> {
    let error = |error: std::io::Error| format!("could not read {path}: {error}");
    let absolute = std::path::absolute(path).map_err(error)?;
    let text = fs::read_to_string(&absolute).map_err(error)?;
    check_credit("body", &text)?;
    absolute
        .into_os_string()
        .into_string()
        .map_err(|_| format!("{path} is not a UTF-8 path"))
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

/// Prints each open issue, with `label` if given, as `<number>\t<title>`.
/// With none, prints nothing on standard output and says so on standard error.
pub fn issue_list(label: Option<&str>) -> Result<()> {
    let gh = Gh::new()?;
    let issues = list_issues(label, |args| {
        let output = gh.output(args)?;
        Ok(Reply {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    })?;
    if issues.is_empty() {
        let labelled = label.map(|label| format!(" labelled {label}"));
        eprintln!("no open issues{}", labelled.unwrap_or_default());
    }
    for issue in issues {
        println!("{issue}");
    }
    Ok(())
}

pub fn issue_comment(number: &str, body_file: &str) -> Result<()> {
    let args = issue_comment_args(number, body_file)?;
    Gh::new()?.run(&args)
}

pub fn issue_new(title: &str, body_file: &str, label: Option<&str>) -> Result<()> {
    let args = issue_new_args(title, body_file, label)?;
    Gh::new()?.run(&args)
}

pub fn pr_open(title: &str, body_file: &str, draft: bool) -> Result<()> {
    let branch = current_branch()?;
    let create = pr_create_args(&branch, title, body_file, draft)?;
    let push = push_args(&branch)?;
    let gh = Gh::new()?;
    crate::run_checked(
        Command::new("git").current_dir(crate::root()).args(&push),
        "git push",
    )?;
    gh.run(&create)
}

pub fn pr_edit(body_file: &str) -> Result<()> {
    checked_body(body_file)?;
    let gh = Gh::new()?;
    let output = gh.output(&pr_number_args())?;
    let number = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || !crate::is_number(&number) {
        return Err("could not find the current branch's pull request".into());
    }
    gh.run(&pr_edit_args(&number, body_file)?)
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

    /// Writes `text` to a file of its own in the system temporary folder and
    /// returns its absolute path.
    fn temp_file(name: &str, text: &str) -> String {
        let path = env::temp_dir().join(format!("bismuth-host-{}-{name}", std::process::id()));
        fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_string()
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
        let body = temp_file("issue.md", "fine\n");
        assert_eq!(
            issue_comment_args("7", &body).unwrap(),
            ["issue", "comment", "7", "--body-file", &body]
        );
        assert_eq!(
            issue_new_args("chore: x", &body, None).unwrap(),
            [
                "issue",
                "create",
                "--title",
                "chore: x",
                "--body-file",
                &body
            ]
        );
        assert_eq!(
            issue_new_args("chore: x", &body, Some("papercut")).unwrap()[6..],
            ["--label", "papercut"]
        );
    }

    #[test]
    fn issue_list_args_ask_for_every_open_issue() {
        let base = [
            "issue",
            "list",
            "--state",
            "open",
            "--limit",
            "1000",
            "--json",
            "number,title",
            "--jq",
            LIST_JQ,
        ];
        assert_eq!(issue_list_args(None), base);
        // Joined to its flag, a label such as `--web` cannot become a flag.
        assert_eq!(issue_list_args(Some("--web"))[10..], ["--label=--web"]);
        assert_eq!(
            label_list_args(),
            [
                "label", "list", "--limit", "1000", "--json", "name", "--jq", ".[].name"
            ]
        );
    }

    #[test]
    fn issue_lines_keep_each_title_on_its_line() {
        // `@tsv` has already turned tab, newline and backslash into `\t`,
        // `\n` and `\\`; any other control character is escaped here.
        let text = "19\tno list\\tcommand\r\n14\tescape \u{1b}[2J and bell \u{7}\n\n";
        assert_eq!(
            issue_lines(text).unwrap(),
            [
                "19\tno list\\tcommand",
                "14\tescape \\u{1b}[2J and bell \\u{7}"
            ]
        );
        assert!(issue_lines("not a number\ttitle\n").is_err());
        assert!(issue_lines("19\n").is_err());
    }

    /// The argument lists a stand-in `gh` was called with.
    type Calls = std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>;

    /// A stand-in for `gh`: answers each call with the next reply, and
    /// records the arguments it was given.
    fn stand_in(replies: Vec<Reply>) -> (Calls, impl FnMut(&[String]) -> Result<Reply>) {
        let calls = Calls::default();
        let seen = calls.clone();
        let mut replies = replies.into_iter();
        let gh = move |args: &[String]| {
            seen.borrow_mut().push(args.to_vec());
            replies
                .next()
                .ok_or_else(|| "gh was called once too often".into())
        };
        (calls, gh)
    }

    fn reply(success: bool, stdout: &str, stderr: &str) -> Reply {
        Reply {
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn open_issues_are_listed() {
        let (calls, gh) = stand_in(vec![reply(true, "19\tlist issues\n7\tx\n", "")]);
        let issues = list_issues(Some("papercut"), gh).unwrap();
        assert_eq!(issues, ["19\tlist issues", "7\tx"]);
        assert_eq!(*calls.borrow(), [issue_list_args(Some("papercut"))]);
    }

    #[test]
    fn no_open_issues_is_an_empty_list() {
        let (calls, gh) = stand_in(vec![reply(true, "", "")]);
        assert_eq!(list_issues(None, gh).unwrap(), Vec::<String>::new());
        assert_eq!(calls.borrow().len(), 1);
        // With a label that exists, in any case, the label list is read too.
        let (calls, gh) = stand_in(vec![
            reply(true, "", ""),
            reply(true, "bug\nPaperCut\n", ""),
        ]);
        assert_eq!(
            list_issues(Some("papercut"), gh).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(calls.borrow()[1], label_list_args());
    }

    #[test]
    fn an_unknown_label_is_an_error() {
        let (_, gh) = stand_in(vec![
            reply(true, "", ""),
            reply(true, "bug\npapercut\n", ""),
        ]);
        let error = list_issues(Some("papercuts"), gh).unwrap_err();
        assert!(error.contains("no label named \"papercuts\""), "{error}");
    }

    #[test]
    fn a_gh_failure_is_an_error_with_its_message() {
        let (_, gh) = stand_in(vec![reply(false, "", "HTTP 401: Bad credentials\n")]);
        let error = list_issues(None, gh).unwrap_err();
        assert_eq!(error, "gh issue list failed: HTTP 401: Bad credentials");
        let (_, gh) = stand_in(vec![reply(true, "", ""), reply(false, "", "HTTP 403\n")]);
        let error = list_issues(Some("papercut"), gh).unwrap_err();
        assert_eq!(error, "gh label list failed: HTTP 403");
    }

    #[test]
    fn push_never_targets_main_or_forces() {
        let feat = "refs/heads/feat/x:refs/heads/feat/x";
        assert_eq!(push_args("feat/x").unwrap(), ["push", "-u", "origin", feat]);
        // A leading `+` would force the push if the name were the refspec.
        assert_eq!(push_args("+x").unwrap()[3], "refs/heads/+x:refs/heads/+x");
        assert!(push_args("main").is_err());
        assert!(push_args("").is_err());
    }

    #[test]
    fn pr_args() {
        let body = temp_file("pr.md", "fine\n");
        assert_eq!(
            pr_create_args("feat/x", "feat: x", &body, false).unwrap(),
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
                &body
            ]
        );
        assert_eq!(
            pr_create_args("feat/x", "feat: x", &body, true)
                .unwrap()
                .last()
                .unwrap(),
            "--draft"
        );
        assert_eq!(
            pr_number_args(),
            ["pr", "view", "--json", "number", "--jq", ".number"]
        );
        let field = format!("body=@{body}");
        assert_eq!(
            pr_edit_args("18", &body).unwrap(),
            [
                "api",
                "--method",
                "PATCH",
                "repos/{owner}/{repo}/pulls/18",
                "-F",
                &field,
                "--jq",
                ".html_url"
            ]
        );
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

    #[test]
    fn every_command_that_sends_text_checks_it_first() {
        let clean = temp_file("clean.md", "fine\n");
        let credit = temp_file("credit.md", "thanks Claude\n");
        assert!(issue_comment_args("7", &credit).is_err());
        assert!(issue_new_args("chore: x", &credit, None).is_err());
        assert!(issue_new_args("chore: thanks Claude", &clean, None).is_err());
        assert!(pr_create_args("feat/x", "feat: x", &credit, false).is_err());
        assert!(pr_create_args("feat/x", "feat: thanks Claude", &clean, false).is_err());
        assert!(pr_edit_args("18", &credit).is_err());
    }

    #[test]
    fn a_relative_body_file_is_sent_as_the_file_that_was_checked() {
        // Tests run in xtask/, while gh runs from the repository root.
        let name = format!("host-test-body-{}.md", std::process::id());
        let here = env::current_dir().unwrap().join(&name);
        let at_root = crate::root().join(&name);
        assert_ne!(here, at_root);
        fs::write(&here, "checked\n").unwrap();
        fs::write(&at_root, "thanks Claude\n").unwrap();
        // Nothing here may panic before both files are removed.
        let sent = pr_edit_args("18", &name).map(|args| {
            let path = args.iter().find_map(|arg| arg.strip_prefix("body=@"));
            path.and_then(|path| fs::read_to_string(crate::root().join(path)).ok())
        });
        let _ = fs::remove_file(&here);
        let _ = fs::remove_file(&at_root);
        assert_eq!(sent, Ok(Some("checked\n".to_string())));
    }

    #[test]
    fn the_token_reaches_gh_only_through_its_environment() {
        let gh = Gh {
            program: "gh".into(),
            credential: Credential::Token(Secret("abc123".into())),
        };
        let command = gh.command(&pr_ready_args());
        assert!(
            command
                .get_args()
                .all(|arg| !arg.to_string_lossy().contains("abc123"))
        );
        let token = command
            .get_envs()
            .find(|(key, _)| *key == "GH_TOKEN")
            .and_then(|(_, value)| value);
        assert_eq!(token, Some(std::ffi::OsStr::new("abc123")));
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
