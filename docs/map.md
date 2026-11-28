# Feature map

What exists, where it lives, and how to exercise it. Read this before
exploring the code. `cargo xtask check` fails if a path named here does not
exist, so update this file in the same pull request that moves or adds
something.

## The system

| Part | Where | What it does | How to exercise it |
|---|---|---|---|
| Boot stage | `boot/src/main.rs` | The program the firmware loads. Prints one line on the serial port and one on the screen, reads the kernel file and checks it with the ELF reader, copies its segments into pages from the firmware, gives them their kernel addresses, prints each result on the serial port, then halts | `cargo xtask test` |
| Kernel copy | `boot/src/load.rs` | Copies each loadable segment of the kernel into cleared pages of its own and records, per segment, the physical and kernel address of its pages, their count and permissions; sums the copied pages | `cargo xtask test` checks the sum against one computed on the host |
| Kernel addresses | `boot/src/map.rs` | Builds new address tables from a copy of the firmware's top-level table plus each kernel segment at its kernel address with its own permissions, switches to them, then walks the tables in use and reads the kernel back through its kernel addresses | `cargo xtask test` checks the `PLACED` line against the file |
| Table memory | `boot/src/memory.rs` | The frames the new tables live in, taken from the firmware in one block, the read of the firmware's top-level table, and the read-back at kernel addresses: the `unsafe` part of the table work | `cargo xtask test` |
| Processor registers | `boot/src/cpu.rs` | Reads CR0, CR3, CR4 and `IA32_EFER`; writes CR3 and `IA32_EFER`; asks `CPUID` whether pages can be marked no-execute | `cargo xtask test` |
| Kernel file report | `boot/src/report.rs` | The lines the boot stage prints about the kernel file and its copy, and the reason for each failure, built without allocation | `cargo xtask check` runs its unit tests through `xtask` |
| Serial output | `boot/src/serial.rs` | Sets up the first serial port and writes text to it | `cargo xtask test` reads what it prints |
| Firmware interface | `boot/src/uefi.rs` | The few UEFI structures the boot stage uses, and the firmware text console | `cargo xtask run` shows the console line |
| Boot services | `boot/src/uefi/boot_services.rs` | Memory and page allocation and protocol lookup through the firmware | `cargo xtask test` |
| Firmware file reading | `boot/src/uefi/file.rs` | Reads a whole file from the disk the boot stage was loaded from | `cargo xtask test` boots disks with a good, missing, empty and damaged kernel file |
| Port access | `boot/src/port.rs` | Reads and writes x86 I/O ports | Used by serial output |
| Kernel | `kernel/src/main.rs` | A separate program, built as an ELF file for the `x86_64-unknown-none` target. Its entry point only stops the processor. The boot stage reads, checks and copies its file and maps it at its address, but does not start it yet | `cargo xtask check` reads its file |
| Processor instructions | `kernel/src/cpu.rs` | The kernel's only `unsafe` code: the halt loop | Used by the kernel |
| ELF reader | `elf/src/lib.rs` | Safe, allocation-free library that checks an ELF file and reports its entry point and loadable segments, or an error. The boot stage uses it on the kernel file | `cargo xtask check` runs its unit tests (`elf/src/tests.rs`) and runs it on the kernel file |
| Placement rules | `elf/src/place.rs` | Part of the ELF reader. The one copy of the rules for placing the kernel at the addresses it is linked for (fixed-address executable, at or above `0xFFFF_FFFF_8000_0000`, no shared pages, at most 16384 pages and 8 segments, not on the last page of the address range, entry point in an executable segment), the pages each segment takes, and the FNV-1a checksum. Used by the boot stage and by `cargo xtask check` | `cargo xtask check` runs its unit tests (`elf/src/place/tests.rs`) |
| Address table builder | `paging/src/lib.rs` | Safe, allocation-free library that builds x86-64 4-level address tables (page tables): maps a range of virtual addresses to physical ones with 4 KiB pages and read, write and execute permissions, in memory the caller lends it, and looks an address up the way the processor does. The boot stage uses it to put the kernel at its address and to check the result | `cargo xtask check` runs its unit tests (`paging/src/tests.rs`) |
| Hand-off structure | `handoff/src/lib.rs` | Safe, allocation-free library for the structure the boot stage will hand the kernel: Bismuth's own memory map, with each firmware memory type turned into one of Bismuth's kinds. The builder takes firmware descriptors in any order, sets ranges aside for the kernel and writes the sorted, merged list; the reader checks any bytes and lists the regions and the usable pages. Nothing uses it yet | `cargo xtask check` runs its unit tests (`handoff/src/tests.rs`) |

## The boot disk

`cargo xtask build` lays it out in the target/esp folder. These paths are
written without code marks because they exist only after a build:

| File | What it is |
|---|---|
| EFI/BOOT/BOOTX64.EFI | The boot stage, which the firmware runs |
| EFI/BISMUTH/KERNEL.ELF | The kernel. The boot stage reads, checks and copies it into memory and maps it at its address, but does not start it yet |

`cargo xtask test` also lays out one disk per boot scenario under
target/boot-scenarios, each with the boot stage and its own kernel file, or
none.

## Building and checking

| Part | Where | What it does |
|---|---|---|
| Build runner | `xtask/src/main.rs` | `build`, `run`, `test`, `snapshot` and `check`, and dispatches the code host commands, `issue-list` among them. Its usage text is the list of commands. `test` runs the boot scenarios. `check` also runs the ELF reader and the placement rules on the kernel file |
| Command table check | `xtask/src/command_tables.rs` | A unit test that fails when the command tables in `AGENTS.md` and `README.md` differ from the commands in the usage text |
| Code host commands | `xtask/src/host.rs` | `pr-open`, `pr-edit`, `pr-ready`, `pr-wait`, `issue-list`, `issue-read`, `issue-comment` and `issue-new`, through `gh`. Checks every title and body with the rule in `tools/check-attribution.sh` before sending it |
| Footprint snapshot | `xtask/src/footprint.rs` | Measures boot image size, `unsafe` counts per file of the boot stage, kernel, ELF reader and address table builder, and third-party packages, and compares the report with the stored one |
| Stored footprint | `snapshots/footprint.txt` | The expected footprint report. `cargo xtask snapshot --update` rewrites it |
| Kernel build settings | `.cargo/config.toml` | Builds the kernel as a fixed-address executable at `0xFFFF_FFFF_8000_0000`, and defines the `cargo xtask` alias |
| Pinned compiler | `rust-toolchain.toml` | The Rust version and the build targets for the boot stage (UEFI) and the kernel |
| Boot checks | `.github/workflows/ci.yml` | Runs `cargo xtask check` on every pull request and on `main`, but not when only a title or description is edited |
| Text checks | `.github/workflows/text.yml` | Runs the authorship, naming and workflow checks below on every pull request and on `main`, and again when a title or description is edited |
| Credit check | `tools/check-attribution.sh` | Fails text that credits a tool |
| Authorship check | `tools/check-history.sh` | Fails commits or pull requests that are not the owner's |
| Naming check | `tools/check-names.sh` | Fails branch names, commit subjects and titles that do not follow the convention |
| Workflow check | `tools/check-workflows.sh` | Fails a workflow that runs the boot test and also re-runs on title or description edits, or does not set `BISMUTH_REQUIRE_QEMU: 1` (which makes a missing QEMU a failure) |
| Commit hook | `.githooks/commit-msg` | Runs the credit and naming checks before a commit is made |

## Documents

| Document | What it covers |
|---|---|
| `docs/decisions.md` | What has been decided, and why |
| `docs/boot.md` | How the system starts |
| `docs/handoff.md` | The structure the boot stage hands the kernel: its layout, the kinds of memory, and the firmware type to kind table |
| `docs/testing.md` | How it is tested, the footprint snapshot, and the planned snapshots |
| `docs/workflow.md` | Who does what, and the path of a task |
| `AGENTS.md` | The rules every contributor follows |

## Agent roles and procedures

| Part | Where |
|---|---|
| Role definitions | `.claude/agents/` |
| Procedures | `.claude/skills/` |
| Pull request template | `.github/pull_request_template.md` |
