# How Bismuth is tested

The aim is that a mistake is caught by a machine before a person has to
notice it. Every kind of test is a `cargo xtask` command, so a developer, an
agent and the pull request checks all run exactly the same thing.

## The layers

| Layer | What it catches | When it runs | Status |
|---|---|---|---|
| Compiler and lints | Type errors, undocumented unsafe code | Every build | In place |
| Unit tests on the development machine | Logic bugs in code that needs no hardware | Every pull request | In place for `xtask` and the ELF reader; the kernel follows |
| Boot scenarios in QEMU | The system not working end to end | Every pull request | Five scenarios, below |
| Before-and-after snapshots | Growth nobody explained | Every pull request | One snapshot, footprint; the rest planned |
| Mutation testing | Tests that still pass when the code is broken | Nightly, and on changed code | Planned, once the kernel has unit tests |
| Miri | Undefined behaviour in unsafe code | Every pull request | Planned, with the first unsafe data structure |
| Fuzzing | Crashes on hostile input | Nightly | Planned. The first parser, the ELF reader, has a fixed-seed mutation test among its unit tests |
| Proofs | Core rules holding for every possible input | Every pull request that touches the core | Planned, with the memory manager |
| Benchmarks with thresholds | Slowdowns | Every pull request | Planned, once there is something to measure |
| A run on real hardware | Whatever QEMU hides | At milestones | Planned |

Nightly jobs run on the code host's own machines. Agents only look at what
fails.

## Boot scenarios

`cargo xtask test` builds the boot stage with the `qemu-exit` feature and
boots it in QEMU once per scenario, each on its own disk under
`target/boot-scenarios`. Each must print `BISMUTH BOOT OK` and then its own
line about the kernel file on the serial port, and QEMU must exit with the
expected code within two minutes:

| Scenario | Kernel file on the disk | Expected line | Exit code |
|---|---|---|---|
| normal | The real kernel | `BISMUTH KERNEL FILE OK entry=0x<hex> segments=<n> bytes=<n>`, with the values the ELF reader finds in the same file on the development machine | 33 |
| missing | None | `BISMUTH KERNEL FILE ERROR: file not found` | 35 |
| empty | Zero bytes | `BISMUTH KERNEL FILE ERROR: file is empty` | 35 |
| bad-signature | The real kernel with its second byte changed | `BISMUTH KERNEL FILE ERROR: ELF reader: BadSignature` | 35 |
| truncated | The real kernel cut in the middle of its last program header | `BISMUTH KERNEL FILE ERROR: ELF reader: ProgramHeadersOutsideFile` | 35 |

Exit code 33 means the boot stage read and checked the kernel file; 35 means
it reported a failure. Both come from the `isa-debug-exit` device, so a
failing scenario ends in seconds instead of waiting for the time limit. All
scenarios run even when one fails, and each prints its name, time and lines.

## Rules

- **Nothing merges with a failing check.**
- **Failing first.** Every change comes with a test that is shown to fail
  without it and pass with it.
- **Every bug gets a regression test before its fix.**
- **A flaky test is a bug.** It is fixed or deleted, never retried until it
  passes.
- **Tests are reviewed like code.** A test that cannot fail is worse than no
  test, because it looks like protection.

## Before-and-after snapshots

A normal test asks whether a feature worked. A snapshot asks what the feature
left behind. The system reports its state as plain, sorted text; the expected
report is stored in the repository; every pull request shows the difference,
and each changed line has to be explained. Growth nobody explained fails the
check.

The footprint snapshot exists; every other snapshot below is planned, and
each arrives with the feature it measures.

### Footprint (in place)

`cargo xtask snapshot` builds the boot image and prints the footprint report.
`cargo xtask snapshot --update` prints it and also stores it in
`snapshots/footprint.txt`. The stored file is the expected report, and it is
committed like any other file.

The report is sorted `key: value` lines. Today it holds:

- `boot image bytes`: the size of the built boot image, `BOOTX64.EFI`, the
  file the firmware loads.
- `third-party packages`: how many packages in `Cargo.lock` come from outside
  the repository. The project has no dependencies, so this is 0.
- `unsafe blocks in <file>` and `unsafe fn declarations in <file>`: for each
  Rust file under `boot/src`, `kernel/src`, `elf/src`, `frames/src`,
  `handoff/src` and `paging/src`, how many `unsafe { ... }` blocks and
  `unsafe fn` declarations it contains. `unsafe` marks code the compiler
  cannot check for memory safety, so its count is a rough measure of how much
  code has to be trusted by hand.

`cargo xtask check` builds the image again, makes the report and compares it
with the stored one before it runs the boot test. If they differ, the check
fails and prints every line that changed: a line that was only in the stored
report starts with `-`, a line that is only in the new one starts with `+`.
A changed value shows as the old line above the new one:

```text
- boot image bytes: 12288
+ boot image bytes: 12800
```

If the change is intended, run `cargo xtask snapshot --update`, commit the new
`snapshots/footprint.txt`, and explain each changed line in the pull request.
If it is not, remove whatever caused it.

Limits to know about:

- The boot image is padded to a multiple of 512 bytes, so its size only
  changes in steps of 512. Growth smaller than that can go unseen. The unsafe
  counts do not have this limit.
- Only `unsafe { ... }` blocks and `unsafe fn` declarations are counted.
  `unsafe impl`, `unsafe extern` blocks and `#[unsafe(...)]` attributes are
  not.
- The counter reads source text, so `unsafe` written out by a macro (a piece
  of code that generates code) is not seen.
- Planned: the work needed to reach the first line of output. It is not
  measured yet.

### Performance

| Snapshot | What it shows |
|---|---|
| Kernel memory | Pages in use per component, and a count of every live kernel object by type |
| Exact leak test | An operation repeated a thousand times must return every counter to where it started |
| Crossing counts | Messages and process switches each operation takes, such as opening a file |
| Instruction counts | Processor instructions an operation executes, which is repeatable where timing is not |

### Security

| Snapshot | What it shows |
|---|---|
| Attack surface | What a change adds: unsafe blocks, system calls, message interfaces, components trusted to share memory with the core, dependencies |
| Permissions | What each process is allowed to touch, before and after an operation |
| Memory map | No memory both writable and executable; no program able to see kernel memory or another program's memory unless it was shared on purpose |
| No leftovers | Memory freed by one process holds none of its contents when handed to another |
| Crash and recover | After a driver is killed and restarted, the snapshot matches the one from before |

## Evidence in a pull request

A pull request does not say "tested". It shows the commands that were run and
what they printed, including the new test failing before the change and
passing after. It says plainly what was not run. The template in
`.github/pull_request_template.md` has a section for this.
