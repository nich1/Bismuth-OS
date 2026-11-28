# Bismuth

Bismuth is an original operating system written in Rust. It is not based on Linux and uses no Linux kernel code.

## Status

Early. A boot stage starts under UEFI firmware in QEMU, prints a line, and reads and checks the kernel file from its disk. The kernel program is built alongside it, but nothing loads it yet.

## Direction

- A microkernel with a small core that can be proved correct
- Drivers and services as separate components that talk through messages
- x86-64, developed in QEMU first and then on real hardware
- A file system and file manager designed together for speed
- A compatibility layer for existing software, later
- A web browser built on top, later

The reasoning behind these is in [docs/decisions.md](docs/decisions.md). How the system starts is explained in [docs/boot.md](docs/boot.md).

## Building and running

You need [Rust](https://rustup.rs) and [QEMU](https://www.qemu.org). The pinned Rust version and the build targets install themselves the first time you run a command here.

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the boot image and the kernel into `target/esp` |
| `cargo xtask run` | Boots it in QEMU with a window and serial output |
| `cargo xtask test` | Boots the boot scenarios in QEMU with no window and checks the output |
| `cargo xtask snapshot` | Builds the boot image and prints the footprint report; `--update` also stores it in `snapshots/footprint.txt` |
| `cargo xtask check` | What the boot checks workflow runs on a pull request: the feature map check, format check, lints, unit tests, the footprint snapshot, the kernel file check and the boot scenarios. Not the text checks |
| `cargo xtask pr-open`, `pr-edit`, `pr-ready`, `pr-wait` | Push the branch and open its pull request, replace its description, mark it ready, wait for its checks |
| `cargo xtask issue-list`, `issue-read`, `issue-comment`, `issue-new` | List the open issues, with `--label` only those with one label; read an issue with its comments, comment on one, file one |

The pull request and issue commands need [gh](https://cli.github.com), GitHub's command-line tool. They use its sign-in, or else the one git already has for github.com. `cargo xtask` with no task lists their arguments.

On Debian or Ubuntu, QEMU's UEFI firmware is a separate package: `sudo apt install qemu-system-x86 ovmf`. On Windows and macOS it comes with QEMU.

## Contributing

See [AGENTS.md](AGENTS.md) for the project's rules, [docs/workflow.md](docs/workflow.md) for how a change gets made, and [docs/testing.md](docs/testing.md) for how it is checked.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
