# Feature map

What exists, where it lives, and how to exercise it. Read this before
exploring the code. `cargo xtask check` fails if a path named here does not
exist, so update this file in the same pull request that moves or adds
something.

## The system

| Part | Where | What it does | How to exercise it |
|---|---|---|---|
| Boot stage | `boot/src/main.rs` | The program the firmware loads. Prints one line on the serial port and one on the screen, then halts | `cargo xtask test` |
| Serial output | `boot/src/serial.rs` | Sets up the first serial port and writes text to it | `cargo xtask test` reads what it prints |
| Firmware interface | `boot/src/uefi.rs` | The few UEFI structures the boot stage uses, and the firmware text console | `cargo xtask run` shows the console line |
| Port access | `boot/src/port.rs` | Reads and writes x86 I/O ports | Used by serial output |
| Kernel | `kernel/src/main.rs` | A separate program, built as an ELF file for the `x86_64-unknown-none` target. Its entry point only stops the processor. Nothing loads it yet | `cargo xtask check` reads its file |
| Processor instructions | `kernel/src/cpu.rs` | The kernel's only `unsafe` code: the halt loop | Used by the kernel |
| ELF reader | `elf/src/lib.rs` | Safe, allocation-free library that checks an ELF file and reports its entry point and loadable segments, or an error. For the boot stage, later | `cargo xtask check` runs its unit tests (`elf/src/tests.rs`) and runs it on the kernel file |

## The boot disk

`cargo xtask build` lays it out in the target/esp folder. These paths are
written without code marks because they exist only after a build:

| File | What it is |
|---|---|
| EFI/BOOT/BOOTX64.EFI | The boot stage, which the firmware runs |
| EFI/BISMUTH/KERNEL.ELF | The kernel. Nothing reads it at boot yet |

## Building and checking

| Part | Where | What it does |
|---|---|---|
| Build runner | `xtask/src/main.rs` | `build`, `run`, `test`, `snapshot` and `check`, and dispatches the code host commands. `check` also runs the ELF reader on the kernel file |
| Code host commands | `xtask/src/host.rs` | `pr-open`, `pr-edit`, `pr-ready`, `pr-wait`, `issue-read`, `issue-comment` and `issue-new`, through `gh`. Checks every title and body with the rule in `tools/check-attribution.sh` before sending it |
| Footprint snapshot | `xtask/src/footprint.rs` | Measures boot image size, `unsafe` counts per file of the boot stage, kernel and ELF reader, and third-party packages, and compares the report with the stored one |
| Stored footprint | `snapshots/footprint.txt` | The expected footprint report. `cargo xtask snapshot --update` rewrites it |
| Pinned compiler | `rust-toolchain.toml` | The Rust version and the build targets for the boot stage (UEFI) and the kernel |
| Boot checks | `.github/workflows/ci.yml` | Runs `cargo xtask check` on every pull request and on `main`, but not when only a title or description is edited |
| Text checks | `.github/workflows/text.yml` | Runs the authorship, naming and workflow checks below on every pull request and on `main`, and again when a title or description is edited |
| Credit check | `tools/check-attribution.sh` | Fails text that credits a tool |
| Authorship check | `tools/check-history.sh` | Fails commits or pull requests that are not the owner's |
| Naming check | `tools/check-names.sh` | Fails branch names, commit subjects and titles that do not follow the convention |
| Workflow check | `tools/check-workflows.sh` | Fails a workflow that re-runs on title or description edits and also runs the boot test |
| Commit hook | `.githooks/commit-msg` | Runs the credit and naming checks before a commit is made |

## Documents

| Document | What it covers |
|---|---|
| `docs/decisions.md` | What has been decided, and why |
| `docs/boot.md` | How the system starts |
| `docs/testing.md` | How it is tested, the footprint snapshot, and the planned snapshots |
| `docs/workflow.md` | Who does what, and the path of a task |
| `AGENTS.md` | The rules every contributor follows |

## Agent roles and procedures

| Part | Where |
|---|---|
| Role definitions | `.claude/agents/` |
| Procedures | `.claude/skills/` |
| Pull request template | `.github/pull_request_template.md` |
