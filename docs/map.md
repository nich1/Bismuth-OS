# Feature map

What exists, where it lives, and how to exercise it. Read this before
exploring the code. `cargo xtask check` fails if a path named here does not
exist, so update this file in the same pull request that moves or adds
something.

## The system

| Part | Where | What it does | How to exercise it |
|---|---|---|---|
| Boot stage | `boot/src/main.rs` | The program the firmware loads. Prints one line on the serial port and one on the screen, reads the kernel file and checks it with the ELF reader, copies its segments into pages from the firmware, gives them their kernel addresses, a stack and a read-only window for the hand-off structure, reads the memory map and ends the firmware's boot services, builds the hand-off structure, prints each result on the serial port, then jumps to the kernel with the structure's address in `rdi`; on a failure it halts instead. Its `damage-handoff` feature, for the boot test only, damages the structure's marker | `cargo xtask test` |
| Leaving the firmware | `boot/src/leave.rs` | The order of the calls that read the memory map and end boot services (`GetMemoryMap`, a buffer with room to spare, the hand-off builder's working space, `ExitBootServices`, one retry on a stale key), over a trait the boot services implement, the reading of the map's descriptors (specific-purpose memory read as reserved), and the count of its entries and usable pages. No firmware code | `cargo xtask check` runs its unit tests through `xtask`, with stand-in calls for each failure; `cargo xtask test` checks the `FIRMWARE LEFT` line |
| Kernel copy | `boot/src/load.rs` | Copies each loadable segment of the kernel into cleared pages of its own and records, per segment, the physical and kernel address of its pages, their count and permissions; sums the copied pages | `cargo xtask test` checks the sum against one computed on the host |
| Building the hand-off | `boot/src/handoff.rs` | Builds the hand-off structure from the saved memory map once boot services have ended: every descriptor, then the kernel's segments, stack and table frames set aside as the kernel's, written into the hand-off window; and the working space that needs. No firmware code | `cargo xtask check` runs its unit tests through `xtask`; `cargo xtask test` checks the kernel's `MEMORY` line |
| Kernel addresses | `boot/src/map.rs` | Builds new address tables from a copy of the firmware's top-level table plus each kernel segment at its kernel address with its own permissions, the kernel's stack below them and the hand-off window below that, read-only, switches to them, then walks the tables in use, reads the kernel back through its kernel addresses, and checks the stack's pages, that its guard page is missing, and the window's pages | `cargo xtask test` checks the `PLACED` line against the file |
| Table memory | `boot/src/memory.rs` | The frames the new tables live in, taken from the firmware in one block, the read of the firmware's top-level table, the read-back at kernel addresses, and the hand-off builder's working space made from pages: the `unsafe` part of the table work | `cargo xtask test` |
| Processor registers | `boot/src/cpu.rs` | Reads CR0, CR3, CR4 and `IA32_EFER`; writes CR3 and `IA32_EFER`; asks `CPUID` whether pages can be marked no-execute; turns interrupts off; jumps to the kernel on its own stack with interrupts off and the hand-off address in `rdi` | `cargo xtask test` |
| Kernel file report | `boot/src/report.rs` | The lines the boot stage prints about the kernel file, its copy, its addresses, leaving the firmware and the hand-off, and the reason for each failure, built without allocation | `cargo xtask check` runs its unit tests through `xtask` |
| Serial output | `boot/src/serial.rs` | Sets up the first serial port and writes text to it | `cargo xtask test` reads what it prints |
| Firmware interface | `boot/src/uefi.rs` | The few UEFI structures the boot stage uses, and the firmware text console | `cargo xtask run` shows the console line |
| Boot services | `boot/src/uefi/boot_services.rs` | Memory and page allocation, protocol lookup, reading the memory map and ending boot services (taking the firmware console with them), through the firmware; in the test build, the boot services pointer read after the exit | `cargo xtask test` |
| Firmware file reading | `boot/src/uefi/file.rs` | Reads a whole file from the disk the boot stage was loaded from | `cargo xtask test` boots disks with a good, missing, empty and damaged kernel file |
| Port access | `boot/src/port.rs` | Reads and writes x86 I/O ports | Used by serial output |
| Kernel | `kernel/src/main.rs` | A separate program, built as an ELF file for the `x86_64-unknown-none` target. The boot stage reads, checks and copies its file, maps it at its address with a stack, and jumps to its entry point. The kernel checks the state it was entered in, reads the hand-off structure and prints `BISMUTH KERNEL MEMORY regions=... usable=... kernel=...`, then `BISMUTH KERNEL OK` on the serial port and stops; a broken entry state prints `BISMUTH KERNEL ENTRY ERROR: ...`, an unusable hand-off `BISMUTH KERNEL HANDOFF ERROR: ...` and its panic handler `BISMUTH KERNEL PANIC`. With `qemu-exit` it then exits QEMU (65, or 67 on any failure); `panic-at-entry` makes it panic once it has printed its `MEMORY` line, for the boot test | `cargo xtask test` boots it; `cargo xtask check` reads its file |
| Processor instructions | `kernel/src/cpu.rs` | The entry stub, which records the stack pointer and flags as the boot stage left them and calls the kernel proper with them and the hand-off address, and the halt loop | Used by the kernel |
| Hand-off window | `kernel/src/handoff.rs` | Makes the hand-off window the boot stage mapped into bytes for the reader: the kernel's one `unsafe` step in reading the structure | `cargo xtask test` |
| Entry checks | `kernel/src/entry.rs` | Checks the recorded entry state against the contract in `docs/boot.md`: interrupts off, direction flag clear, stack pointer aligned for a function entry and in the top half, and the hand-off address the contract's window; counts the hand-off structure's regions, usable pages and kernel pages; writes decimal numbers. No hardware code | `cargo xtask check` runs its unit tests through `xtask`; `cargo xtask test` boots it |
| Kernel port access | `kernel/src/port.rs` | Reads and writes x86 I/O ports, and with `qemu-exit` makes QEMU exit | Used by the kernel's serial output |
| Kernel serial output | `kernel/src/serial.rs` | Sets up the first serial port again and writes text to it | `cargo xtask test` reads what it prints |
| ELF reader | `elf/src/lib.rs` | Safe, allocation-free library that checks an ELF file and reports its entry point and loadable segments, or an error. The boot stage uses it on the kernel file | `cargo xtask check` runs its unit tests (`elf/src/tests.rs`) and runs it on the kernel file |
| Placement rules | `elf/src/place.rs` | Part of the ELF reader. The one copy of the rules for placing the kernel at the addresses it is linked for (fixed-address executable, at or above `0xFFFF_FFFF_8000_0000`, no shared pages, at most 16384 pages and 8 segments, not on the last page of the address range, entry point in an executable segment), the pages each segment takes, the FNV-1a checksum, where the kernel's stack and its guard page go (just below the lowest address a segment may use), where the hand-off window goes (below the guard page), and the number of table frames the boot stage takes. Used by the boot stage and by `cargo xtask check` | `cargo xtask check` runs its unit tests (`elf/src/place/tests.rs`) |
| Frame tracker | `frames/src/lib.rs` | Safe, allocation-free library that keeps track of which 4 KiB frames of a range of physical memory are free, in a bitmap the caller lends it: adds usable ranges, reserves taken ones, hands out the lowest free frame and takes it back, refuses bad calls with an error, and keeps exact counts with a recount to check them against. Not used by the boot stage or the kernel yet | `cargo xtask check` runs its unit tests (`frames/src/tests.rs`) |
| Address table builder | `paging/src/lib.rs` | Safe, allocation-free library that builds x86-64 4-level address tables (page tables): maps a range of virtual addresses to physical ones with 4 KiB pages and read, write and execute permissions, in memory the caller lends it, and looks an address up the way the processor does. The boot stage uses it to put the kernel at its address and to check the result | `cargo xtask check` runs its unit tests (`paging/src/tests.rs`) |
| Hand-off structure | `handoff/src/lib.rs` | Safe, allocation-free library for the structure the boot stage will hand the kernel: Bismuth's own memory map, with each firmware memory type turned into one of Bismuth's kinds. The builder takes firmware descriptors in any order, sets ranges aside for the kernel and writes the sorted, merged list; the reader checks any bytes and lists the regions and the usable pages. The boot stage writes the structure with it and the kernel reads it | `cargo xtask check` runs its unit tests (`handoff/src/tests.rs`) |
| Descriptor tables | `descriptors/src/lib.rs` | Safe, allocation-free library with no hardware access that encodes and decodes the x86-64 descriptor table entries: segment selectors, the segment table (GDT) entries in the order the kernel will use (null, kernel code, kernel data, user data, user code, then the TSS descriptor), the 64-bit task state segment as 104 bytes with accessors, interrupt and trap gates, the 10-byte operand of `lgdt` and `lidt`, and the table of exceptions 0 to 31 with their error codes and classes. Refuses bad values with an error. Nothing uses it yet | `cargo xtask check` runs its unit tests (`descriptors/src/tests.rs`) |

## The boot disk

`cargo xtask build` lays it out in the target/esp folder. These paths are
written without code marks because they exist only after a build:

| File | What it is |
|---|---|
| EFI/BOOT/BOOTX64.EFI | The boot stage, which the firmware runs |
| EFI/BISMUTH/KERNEL.ELF | The kernel. The boot stage reads, checks and copies it into memory, maps it at its address and starts it |

`cargo xtask test` also lays out one disk per boot scenario under
target/boot-scenarios, each with the boot stage and its own kernel file, or
none. One of those kernel files is built to panic on entry, and one disk
gets a boot stage built to damage the hand-off structure (kept in
target/boot-damaged).

## Building and checking

| Part | Where | What it does |
|---|---|---|
| Build runner | `xtask/src/main.rs` | `build`, `run`, `test`, `snapshot` and `check`, and dispatches the code host commands, `issue-list` among them. Its usage text is the list of commands. `test` runs the boot scenarios. `check` also runs the ELF reader and the placement rules on the kernel file |
| Command table check | `xtask/src/command_tables.rs` | A unit test that fails when the command tables in `AGENTS.md` and `README.md` differ from the commands in the usage text |
| Code host commands | `xtask/src/host.rs` | `pr-open`, `pr-edit`, `pr-ready`, `pr-wait`, `issue-list`, `issue-read`, `issue-comment` and `issue-new`, through `gh`. Checks every title and body with the rule in `tools/check-attribution.sh` before sending it |
| Footprint snapshot | `xtask/src/footprint.rs` | Measures boot image size, `unsafe` counts per file of the boot stage, the kernel and the libraries, and third-party packages, and compares the report with the stored one |
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
