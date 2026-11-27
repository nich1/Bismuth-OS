//! Build, run and test tasks for Bismuth.
//!
//! Run with `cargo xtask <task>`. This is an ordinary program for the
//! development machine, so the same commands work on Windows, Linux and macOS
//! without shell scripts.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod footprint;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  build   build the boot image into target/esp
  run     build, then boot it in QEMU with a window and serial output
  test    build, boot it in QEMU without a window, and check what it printed
  snapshot [--update]
          build, then print the footprint report; --update also stores it
          in snapshots/footprint.txt
  check   everything the pull request check runs: format, lint, unit tests,
          footprint snapshot, boot test";

/// The Rust target for programs the UEFI firmware can load.
const UEFI_TARGET: &str = "x86_64-unknown-uefi";
/// The package that becomes the boot image.
const BOOT_PACKAGE: &str = "bismuth-boot";
/// The text the boot stage must print on the serial port.
const BOOT_MARKER: &str = "BISMUTH BOOT OK";
/// What QEMU exits with when the boot stage reports success: `(0x10 << 1) | 1`.
const QEMU_SUCCESS_EXIT: i32 = 33;
/// How long the boot test may take before it is treated as a hang.
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);

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

/// Builds the boot stage and lays it out as a bootable disk in `target/esp`.
///
/// UEFI firmware boots a disk by running `EFI/BOOT/BOOTX64.EFI` from it, so
/// that is where the built program is copied. With `exit_when_done`, the boot
/// stage is built to make QEMU quit once it has printed its message.
fn build(exit_when_done: bool) -> Result<()> {
    let mut command = cargo();
    command.args(["build", "--package", BOOT_PACKAGE, "--target", UEFI_TARGET]);
    if exit_when_done {
        command.args(["--features", "qemu-exit"]);
    }
    run_checked(&mut command, "cargo build")?;

    let root = root();
    let built = root
        .join("target")
        .join(UEFI_TARGET)
        .join("debug")
        .join(format!("{BOOT_PACKAGE}.efi"));
    let image = boot_image();
    let boot_dir = image.parent().expect("the boot image path has a folder");
    fs::create_dir_all(boot_dir)
        .map_err(|error| format!("could not create {}: {error}", boot_dir.display()))?;
    fs::copy(&built, &image)
        .map_err(|error| format!("could not copy {}: {error}", built.display()))?;
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
    let mut qemu = qemu_command()?;
    qemu.args(["-serial", "stdio"]);
    run_checked(&mut qemu, "QEMU")
}

/// Boots the image in QEMU with no window and checks the result.
///
/// The test passes when the serial output contains [`BOOT_MARKER`] and QEMU
/// exits with [`QEMU_SUCCESS_EXIT`] within [`BOOT_TIMEOUT`].
fn test() -> Result<()> {
    build(true)?;
    let mut qemu = qemu_command()?;
    qemu.args(["-serial", "stdio", "-display", "none", "-no-reboot"])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());

    let mut child = qemu
        .spawn()
        .map_err(|error| format!("could not start QEMU: {error}"))?;
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error only shortens the captured output; the checks below
        // still decide the result.
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });

    let deadline = Instant::now() + BOOT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                // Killing can only fail if QEMU has just exited by itself.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("could not wait for QEMU: {error}")),
        }
    };
    let serial = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();

    let failure = match status {
        None => Some(format!(
            "QEMU did not exit within {} seconds",
            BOOT_TIMEOUT.as_secs()
        )),
        Some(status) if status.code() != Some(QEMU_SUCCESS_EXIT) => Some(format!(
            "QEMU exited with {status}, expected exit code {QEMU_SUCCESS_EXIT}"
        )),
        Some(_) if !serial.contains(BOOT_MARKER) => {
            Some(format!("serial output did not contain {BOOT_MARKER:?}"))
        }
        Some(_) => None,
    };
    match failure {
        None => {
            println!("boot test passed: serial output contained {BOOT_MARKER:?}");
            Ok(())
        }
        Some(reason) => Err(format!(
            "boot test failed: {reason}\n--- serial output ---\n{serial}\n--- end ---"
        )),
    }
}

/// Runs every check the pull request pipeline runs.
fn check() -> Result<()> {
    check_map()?;
    run_checked(cargo().args(["fmt", "--all", "--check"]), "cargo fmt")?;
    run_checked(
        cargo().args(["test", "--package", "xtask"]),
        "cargo test (xtask)",
    )?;
    run_checked(
        cargo().args(["clippy", "--package", "xtask", "--", "-D", "warnings"]),
        "cargo clippy (xtask)",
    )?;
    for features in [&[][..], &["--features", "qemu-exit"][..]] {
        run_checked(
            cargo()
                .args(["clippy", "--package", BOOT_PACKAGE, "--target", UEFI_TARGET])
                .args(features)
                .args(["--", "-D", "warnings"]),
            "cargo clippy (boot)",
        )?;
    }
    check_snapshot()?;
    test()
}

/// A QEMU command set up to boot `target/esp` through UEFI firmware.
///
/// It runs from the repository root so the disk can be given as a relative
/// path, which avoids quoting problems with drive letters and spaces.
fn qemu_command() -> Result<Command> {
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
    command.args(["-drive", "format=raw,file=fat:rw:target/esp"]);
    Ok(command)
}

/// Finds the QEMU program: `BISMUTH_QEMU`, then `PATH`, then the default
/// Windows install folder.
fn find_qemu() -> Result<PathBuf> {
    if let Some(path) = env::var_os("BISMUTH_QEMU") {
        return Ok(path.into());
    }
    let name = "qemu-system-x86_64";
    let on_path = Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if on_path {
        return Ok(name.into());
    }
    let windows_default = Path::new(r"C:\Program Files\qemu\qemu-system-x86_64.exe");
    if windows_default.is_file() {
        return Ok(windows_default.to_path_buf());
    }
    Err(format!(
        "could not find {name}. Install QEMU, or set BISMUTH_QEMU to its path"
    ))
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

/// Fails if the feature map names a file or folder that does not exist.
///
/// A map that points at things that have moved misleads whoever reads it, so
/// this keeps it from going stale without anyone noticing.
fn check_map() -> Result<()> {
    let root = root();
    let text = fs::read_to_string(root.join(MAP_FILE))
        .map_err(|error| format!("could not read {MAP_FILE}: {error}"))?;
    let missing: Vec<&str> = paths_in_markdown(&text)
        .into_iter()
        .filter(|path| !root.join(path).exists())
        .collect();
    if missing.is_empty() {
        Ok(())
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
    use super::paths_in_markdown;

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
