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
          format, lint, unit tests, footprint snapshot, kernel file check,
          boot scenarios (not the text checks)

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
    if let Err(error) = place::place(&elf) {
        return Err(format!(
            "the kernel cannot be placed as linked: {error:?}\n{text}"
        ));
    }
    match elf.entry_segment() {
        Some(segment) if segment.flags.execute => Ok(text),
        _ => Err(format!(
            "the kernel's entry point is not in an executable segment:\n{text}"
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
    let mut normal = Scenario::new("normal", Some(kernel), ok, QEMU_SUCCESS_EXIT);
    normal.lines.push(copied_line(kernel, &elf));

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
    for (package, name) in [("bismuth-elf", "elf"), ("bismuth-paging", "paging")] {
        run_checked(
            cargo().args(["test", "--package", package]),
            &format!("cargo test ({name})"),
        )?;
        run_checked(
            cargo().args([
                "clippy",
                "--package",
                package,
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ]),
            &format!("cargo clippy ({name})"),
        )?;
    }
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
    use super::{
        Scenario, boot_failure, describe_kernel, page_images, paths_in_markdown, scenarios,
    };
    use bismuth_elf::place::{Error as PlaceError, KERNEL_BASE, PAGE_LIMIT};

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
                "too-large"
            ]
        );
        assert_eq!(
            all[0].lines[1],
            "BISMUTH KERNEL FILE OK entry=0xffffffff80001040 segments=2 bytes=8192"
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
