# How Bismuth is tested

The aim is that a mistake is caught by a machine before a person has to
notice it. Every kind of test is a `cargo xtask` command, so a developer, an
agent and the pull request checks all run exactly the same thing.

## The layers

| Layer | What it catches | When it runs | Status |
|---|---|---|---|
| Compiler and lints | Type errors, undocumented unsafe code | Every build | In place |
| Unit tests on the development machine | Logic bugs in code that needs no hardware | Every pull request | In place for `xtask`; the kernel follows |
| Boot scenarios in QEMU | The system not working end to end | Every pull request | One scenario |
| Mutation testing | Tests that still pass when the code is broken | Nightly, and on changed code | Planned, once the kernel has unit tests |
| Miri | Undefined behaviour in unsafe code | Every pull request | Planned, with the first unsafe data structure |
| Fuzzing | Crashes on hostile input | Nightly | Planned, with the first parser or system call |
| Proofs | Core rules holding for every possible input | Every pull request that touches the core | Planned, with the memory manager |
| Benchmarks with thresholds | Slowdowns | Every pull request | Planned, once there is something to measure |
| A run on real hardware | Whatever QEMU hides | At milestones | Planned |

Nightly jobs run on the code host's own machines. Agents only look at what
fails.

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

The planned command is `cargo xtask snapshot`. None of these exist yet; each
arrives with the feature it measures.

### Performance

| Snapshot | What it shows |
|---|---|
| Footprint | Size of the boot image, and the work needed to reach the first line of output |
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
