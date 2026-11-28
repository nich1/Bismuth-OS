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

// The boot stage's report text has no firmware code, so its unit tests run
// here, on the development machine, where the boot stage itself cannot.
#[cfg(test)]
#[path = "../../boot/src/report.rs"]
mod boot_report;

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
  check   everything the pull request check runs: format, lint, unit tests,
          footprint snapshot, kernel file check, boot scenarios

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
/// What QEMU exits with when the boot stage reports that the kernel file is
/// missing or wrong: `(0x11 << 1) | 1`.
const QEMU_FAILURE_EXIT: i32 = 35;
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

/// Fails unless the kernel file has at least one loadable segment, a layout
/// with no [`layout_problems`] and an entry point inside an executable
/// segment, and describes the file.
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
    let segments: Vec<_> = elf.segments().collect();
    let problems = layout_problems(elf.file_type(), elf.entry(), &segments);
    if !problems.is_empty() {
        return Err(format!(
            "the kernel cannot be mapped as linked:\n  {}\n{text}",
            problems.join("\n  ")
        ));
    }
    match elf.entry_segment() {
        Some(segment) if segment.flags.execute => Ok(text),
        _ => Err(format!(
            "the kernel's entry point is not in an executable segment:\n{text}"
        )),
    }
}

/// The lowest address the kernel may use: the start of the top 2 GiB of the
/// address range, where the kernel is linked (see `docs/decisions.md`).
const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;
/// The page size the kernel's segments will be mapped with.
const PAGE_SIZE: u64 = 4096;

/// Says what in the kernel's layout stops it from being mapped as linked, one
/// line per problem, or nothing if all is well.
fn layout_problems(
    file_type: bismuth_elf::FileType,
    entry: u64,
    segments: &[bismuth_elf::Segment],
) -> Vec<String> {
    let mut problems = Vec::new();
    // gABI ch. 4, "ELF Header", `e_type`: only `ET_EXEC` runs at the
    // addresses it names, with no relocation.
    if file_type != bismuth_elf::FileType::Executable {
        problems.push(format!(
            "the file is {file_type:?}, not a fixed-address executable (ET_EXEC)"
        ));
    }
    if entry < KERNEL_BASE {
        problems.push(format!(
            "the entry point {entry:#x} is below {KERNEL_BASE:#x}"
        ));
    }
    for (index, segment) in segments.iter().enumerate() {
        if segment.address < KERNEL_BASE {
            problems.push(format!(
                "segment {index} at {:#x} is below {KERNEL_BASE:#x}",
                segment.address
            ));
        }
        // gABI ch. 5, "Program Loading": a segment is mapped page by page
        // straight from the file, which needs its address and offset at the
        // same place in a page.
        if segment.address % PAGE_SIZE != segment.offset % PAGE_SIZE {
            problems.push(format!(
                "segment {index} at {:#x} is at offset {:#x} in the file, a different \
                 place in its 4 KiB page",
                segment.address, segment.offset
            ));
        }
    }
    // Each segment gets its own permissions, so no page may hold two.
    let pages = |segment: &bismuth_elf::Segment| {
        let last = segment
            .address
            .checked_add(segment.memory_size)?
            .checked_sub(1)?;
        (segment.memory_size > 0).then_some((segment.address / PAGE_SIZE, last / PAGE_SIZE))
    };
    for (first, a) in segments.iter().enumerate() {
        for (second, b) in segments.iter().enumerate().skip(first + 1) {
            if let (Some((a_start, a_end)), Some((b_start, b_end))) = (pages(a), pages(b))
                && a_start <= b_end
                && b_start <= a_end
            {
                problems.push(format!(
                    "segments {first} and {second} share the 4 KiB page at {:#x}",
                    a_start.max(b_start) * PAGE_SIZE
                ));
            }
        }
    }
    problems
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
    /// The exit code QEMU must end with.
    exit_code: i32,
}

impl Scenario {
    fn new(name: &'static str, kernel: Option<&[u8]>, line: String, exit_code: i32) -> Self {
        Scenario {
            name,
            kernel: kernel.map(<[u8]>::to_vec),
            lines: vec![BOOT_MARKER.to_string(), line],
            exit_code,
        }
    }

    fn error(name: &'static str, kernel: Option<&[u8]>, reason: &str) -> Self {
        let line = format!("BISMUTH KERNEL FILE ERROR: {reason}");
        Scenario::new(name, kernel, line, QEMU_FAILURE_EXIT)
    }
}

/// The boot scenarios, built from the real kernel file `kernel`.
///
/// The normal one expects the entry point and loadable segment count that
/// the ELF reader finds in the same file here, as [`check_kernel`] reports.
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
    Ok(vec![
        Scenario::new("normal", Some(kernel), ok, QEMU_SUCCESS_EXIT),
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
    ])
}

/// Builds the boot stage with `qemu-exit`, runs every boot scenario and
/// fails if any did not behave as expected.
fn test() -> Result<()> {
    build(true)?;
    let kernel = fs::read(kernel_image())
        .map_err(|error| format!("could not read the kernel file: {error}"))?;
    let mut failed = Vec::new();
    for scenario in scenarios(&kernel)? {
        let started = Instant::now();
        let disk = scenario_disk(&scenario)?;
        let (status, serial) = boot(&disk)?;
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
    copy_file(&boot_image(), &efi.join("BOOT").join("BOOTX64.EFI"))?;
    if let Some(kernel) = &scenario.kernel {
        let folder = efi.join("BISMUTH");
        fs::create_dir_all(&folder)
            .map_err(|error| format!("could not create {}: {error}", folder.display()))?;
        fs::write(folder.join("KERNEL.ELF"), kernel)
            .map_err(|error| format!("could not write the kernel into {disk}: {error}"))?;
    }
    Ok(disk)
}

/// Says why a boot that ended with exit code `status` (`None` for a hang)
/// and printed `serial` does not meet `scenario`, or `None` if it does.
fn boot_failure(scenario: &Scenario, status: Option<i32>, serial: &str) -> Option<String> {
    let Some(code) = status else {
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
    for line in &scenario.lines {
        if !printed.any(|printed| printed == line) {
            return Some(format!("serial output did not contain {line:?} in order"));
        }
    }
    None
}

/// Boots the folder `disk` in QEMU with no window. Returns QEMU's exit code,
/// or `None` if it ran past [`BOOT_TIMEOUT`] or was stopped by a signal, and
/// the serial output.
fn boot(disk: &str) -> Result<(Option<i32>, String)> {
    let mut qemu = qemu_command(disk)?;
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
            Ok(Some(status)) => break status.code(),
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
    Ok((status, serial))
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
        cargo_step("unit tests (elf)", vec!["test", "--package", "bismuth-elf"]),
        cargo_step("lints (elf)", lints("bismuth-elf", &["--all-targets"])),
        cargo_step(
            "unit tests (paging)",
            vec!["test", "--package", "bismuth-paging"],
        ),
        cargo_step(
            "lints (paging)",
            lints("bismuth-paging", &["--all-targets"]),
        ),
        cargo_step(
            "lints (kernel)",
            lints(KERNEL_PACKAGE, &["--target", KERNEL_TARGET]),
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
        KERNEL_BASE, Result, Scenario, Step, Stop, boot_failure, describe_kernel, layout_problems,
        locate_qemu, paths_in_markdown, run_steps, scenarios,
    };
    use bismuth_elf::FileType::{Executable, SharedObject};
    use bismuth_elf::{Flags, Segment};
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
        assert!(failure(Some(33), serial).contains("code 33, expected 35"));
        assert!(failure(Some(35), "BISMUTH BOOT OK\n").contains("did not contain"));
        // Out of order, or only part of a line.
        let reversed = "BISMUTH KERNEL FILE ERROR: file not found\nBISMUTH BOOT OK\n";
        assert!(failure(Some(35), reversed).contains("in order"));
        let partial = "BISMUTH BOOT OK\nBISMUTH KERNEL FILE ERROR: file not found here\n";
        assert!(failure(Some(35), partial).contains("did not contain"));
    }

    #[test]
    fn the_scenarios_damage_the_kernel_as_named() {
        let kernel = kernel_file(0b101, KERNEL_BASE + 0x40, 1);
        let all = scenarios(&kernel).expect("scenarios");
        let names: Vec<_> = all.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            ["normal", "missing", "empty", "bad-signature", "truncated"]
        );
        assert_eq!(
            all[0].lines[1],
            "BISMUTH KERNEL FILE OK entry=0xffffffff80000040 segments=1 bytes=4096"
        );
        assert_eq!(all[0].exit_code, 33);
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
    }

    /// A 4 KiB x86-64 ELF executable whose one program header is a loadable
    /// segment with `flags`, holding the whole file at [`KERNEL_BASE`], and
    /// whose entry is `entry`. Layouts from the gABI "ELF Header" and
    /// "Program Header" sections.
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
        assert!(error.contains("not in an executable segment"), "{error}");
    }

    #[test]
    fn a_kernel_entered_outside_every_segment_fails() {
        let error =
            describe_kernel(&kernel_file(0b101, KERNEL_BASE + 0x1000, 1)).expect_err("fails");
        assert!(error.contains("not in an executable segment"), "{error}");
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

    /// A loadable segment at `address`, read from `offset`, taking `memory`
    /// bytes, with the kernel linker's 4 KiB alignment.
    fn segment(address: u64, offset: u64, memory: u64, execute: bool) -> Segment {
        Segment {
            offset,
            file_size: memory,
            address,
            memory_size: memory,
            align: 0x1000,
            flags: Flags {
                read: true,
                write: !execute,
                execute,
            },
        }
    }

    /// The shape the kernel is linked to: read-only, code and data segments
    /// on pages of their own, from [`KERNEL_BASE`] up.
    fn good_layout() -> Vec<Segment> {
        vec![
            segment(KERNEL_BASE, 0, 0x274, false),
            segment(KERNEL_BASE + 0x1280, 0x280, 0x15, true),
            segment(KERNEL_BASE + 0x2298, 0x298, 0xd68, false),
        ]
    }

    #[test]
    fn the_kernel_layout_passes() {
        let problems = layout_problems(Executable, KERNEL_BASE + 0x1280, &good_layout());
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_position_independent_kernel_fails() {
        let problems = layout_problems(SharedObject, KERNEL_BASE + 0x1280, &good_layout());
        assert_eq!(
            problems,
            ["the file is SharedObject, not a fixed-address executable (ET_EXEC)"]
        );
    }

    #[test]
    fn a_kernel_entered_below_the_top_2_gib_fails() {
        let problems = layout_problems(Executable, 0x1280, &good_layout());
        assert_eq!(
            problems,
            ["the entry point 0x1280 is below 0xffffffff80000000"]
        );
    }

    #[test]
    fn a_segment_below_the_top_2_gib_fails() {
        let mut segments = good_layout();
        segments[0].address = KERNEL_BASE - 0x1000;
        let problems = layout_problems(Executable, KERNEL_BASE + 0x1280, &segments);
        assert_eq!(
            problems,
            ["segment 0 at 0xffffffff7ffff000 is below 0xffffffff80000000"]
        );
    }

    #[test]
    fn a_segment_whose_address_and_offset_disagree_fails() {
        let mut segments = good_layout();
        segments[1].offset = 0x290;
        let problems = layout_problems(Executable, KERNEL_BASE + 0x1280, &segments);
        assert_eq!(
            problems,
            [
                "segment 1 at 0xffffffff80001280 is at offset 0x290 in the file, \
                 a different place in its 4 KiB page"
            ]
        );
    }

    #[test]
    fn segments_sharing_a_page_fail() {
        let mut segments = good_layout();
        // The code now starts on the read-only segment's last page.
        segments[1].address = KERNEL_BASE + 0x280;
        let problems = layout_problems(Executable, KERNEL_BASE + 0x280, &segments);
        assert_eq!(
            problems,
            ["segments 0 and 1 share the 4 KiB page at 0xffffffff80000000"]
        );
    }

    #[test]
    fn segments_ending_and_starting_on_page_edges_do_not_share() {
        let segments = [
            segment(KERNEL_BASE, 0, 0x1000, false),
            segment(KERNEL_BASE + 0x1000, 0x1000, 0x1000, true),
            // Takes no memory, so no page.
            segment(KERNEL_BASE + 0x1000, 0x1000, 0, false),
        ];
        let problems = layout_problems(Executable, KERNEL_BASE + 0x1000, &segments);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_kernel_linked_low_fails_the_file_check() {
        // gABI ch. 5, "Program Header": `p_vaddr` is the eight bytes at 16.
        let mut file = kernel_file(0b101, 0x20_0040, 1);
        file[80..88].copy_from_slice(&0x20_0000u64.to_le_bytes());
        let error = describe_kernel(&file).expect_err("fails");
        assert!(
            error.starts_with(
                "the kernel cannot be mapped as linked:\n  \
                 the entry point 0x200040 is below 0xffffffff80000000\n  \
                 segment 0 at 0x200000 is below 0xffffffff80000000\n"
            ),
            "{error}"
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
