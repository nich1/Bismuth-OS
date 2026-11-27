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
mod host;

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  build   build the boot image and the kernel into target/esp
  run     build, then boot it in QEMU with a window and serial output
  test    build, boot it in QEMU without a window, and check what it printed
  snapshot [--update]
          build, then print the footprint report; --update also stores it
          in snapshots/footprint.txt
  check   everything the pull request check runs: format, lint, unit tests,
          footprint snapshot, kernel file check, boot test

code host tasks (need gh, GitHub's command-line tool):
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
/// `EFI/BISMUTH/KERNEL.ELF`. With `exit_when_done`, the boot stage is built
/// to make QEMU quit once it has printed its message.
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
    copy_file(&built, &image)?;

    run_checked(
        cargo().args([
            "build",
            "--package",
            KERNEL_PACKAGE,
            "--target",
            KERNEL_TARGET,
        ]),
        "cargo build (kernel)",
    )?;
    let built = root
        .join("target")
        .join(KERNEL_TARGET)
        .join("debug")
        .join(KERNEL_PACKAGE);
    copy_file(&built, &kernel_image())
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

/// Fails unless the kernel file has at least one loadable segment and an
/// entry point inside an executable one, and describes the file.
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
    match elf.entry_segment() {
        Some(segment) if segment.flags.execute => Ok(text),
        _ => Err(format!(
            "the kernel's entry point is not in an executable segment:\n{text}"
        )),
    }
}

/// Runs the ELF reader on the kernel file [`build`] produced.
fn check_kernel() -> Result<()> {
    let path = kernel_image();
    let bytes =
        fs::read(&path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    print!("kernel file check passed: {}", describe_kernel(&bytes)?);
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
    run_checked(
        cargo().args(["test", "--package", "bismuth-elf"]),
        "cargo test (elf)",
    )?;
    run_checked(
        cargo().args([
            "clippy",
            "--package",
            "bismuth-elf",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
        "cargo clippy (elf)",
    )?;
    run_checked(
        cargo()
            .args([
                "clippy",
                "--package",
                KERNEL_PACKAGE,
                "--target",
                KERNEL_TARGET,
            ])
            .args(["--", "-D", "warnings"]),
        "cargo clippy (kernel)",
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
    check_kernel()?;
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
    use super::{describe_kernel, paths_in_markdown};

    /// A 4 KiB x86-64 ELF executable whose one program header is a loadable
    /// segment with `flags`, holding the whole file at 0x20_0000, and whose
    /// entry is `entry`. Layouts from the gABI "ELF Header" and "Program
    /// Header" sections.
    fn kernel_file(flags: u32, entry: u64, headers: u16) -> Vec<u8> {
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
        put(80, &0x20_0000u64.to_le_bytes());
        put(96, &4096u64.to_le_bytes());
        put(104, &4096u64.to_le_bytes());
        put(112, &4096u64.to_le_bytes());
        file
    }

    #[test]
    fn a_kernel_entered_in_an_executable_segment_passes() {
        let text = describe_kernel(&kernel_file(0b101, 0x20_0040, 1)).expect("passes");
        assert_eq!(
            text,
            "Executable, entry 0x200040, 4096 bytes\n  load 0x200000: 0x1000 bytes from \
             offset 0x0, 0x1000 in memory, align 0x1000, r-x\n"
        );
    }

    #[test]
    fn a_kernel_entered_in_a_data_segment_fails() {
        let error = describe_kernel(&kernel_file(0b110, 0x20_0040, 1)).expect_err("fails");
        assert!(error.contains("not in an executable segment"), "{error}");
    }

    #[test]
    fn a_kernel_entered_outside_every_segment_fails() {
        let error = describe_kernel(&kernel_file(0b101, 0x20_1000, 1)).expect_err("fails");
        assert!(error.contains("not in an executable segment"), "{error}");
    }

    #[test]
    fn a_kernel_without_loadable_segments_fails() {
        let error = describe_kernel(&kernel_file(0b101, 0x20_0040, 0)).expect_err("fails");
        assert!(error.contains("no loadable segment"), "{error}");
    }

    #[test]
    fn a_file_the_reader_rejects_fails() {
        let error = describe_kernel(&[0x7F, b'E', b'L', b'F']).expect_err("fails");
        assert_eq!(error, "the ELF reader rejected the kernel: Truncated");
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
