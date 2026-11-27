# Bismuth

Bismuth is an original operating system written in Rust. It is not based on Linux and uses no Linux kernel code.

## Status

Design phase. Nothing boots yet.

## Direction

- A microkernel with a small core that can be proved correct
- Drivers and services as separate components that talk through messages
- x86-64, developed in QEMU first and then on real hardware
- A file system and file manager designed together for speed
- A compatibility layer for existing software, later
- A web browser built on top, later

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
