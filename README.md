# Bismuth

Bismuth is an original operating system written in Rust. It is not based on Linux and uses no Linux kernel code.

## Status

Early. A boot stage starts under UEFI firmware in QEMU and prints a line. There is no kernel yet.

## Direction

- A microkernel with a small core that can be proved correct
- Drivers and services as separate components that talk through messages
- x86-64, developed in QEMU first and then on real hardware
- A file system and file manager designed together for speed
- A compatibility layer for existing software, later
- A web browser built on top, later

The reasoning behind these is in [docs/decisions.md](docs/decisions.md). How the system starts is explained in [docs/boot.md](docs/boot.md).

## Building and running

You need [Rust](https://rustup.rs) and [QEMU](https://www.qemu.org). The pinned Rust version and the UEFI build target install themselves the first time you run a command here.

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the boot image into `target/esp` |
| `cargo xtask run` | Boots it in QEMU with a window and serial output |
| `cargo xtask test` | Boots it in QEMU with no window and checks the output |
| `cargo xtask check` | Format check, lints and the boot test |

On Debian or Ubuntu, QEMU's UEFI firmware is a separate package: `sudo apt install qemu-system-x86 ovmf`. On Windows and macOS it comes with QEMU.

## Contributing

See [AGENTS.md](AGENTS.md) for the project's rules and pull request workflow.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
