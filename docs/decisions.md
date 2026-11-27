# Decisions

What has been decided about Bismuth, and why. New entries go at the bottom of
the relevant section with the date. An entry changes only with the owner's
agreement.

## Goals

- **An original stack, built for its own sake.** Bismuth, then a web browser
  on top of it, possibly a programming language much later. The aim is to
  make it ourselves and make it good, not to compete as a product.
  (2026-11-27)
- **Fast where it is felt.** The target is "as fast as the best, and
  measured", plus a desktop that always responds at once. Bismuth will not be
  the fastest at everything. (2026-11-27)
- **A file system and file manager faster than Windows.** Designed together:
  instant search by name, folder sizes tracked as files change, instant
  copies, and whole-folder requests in one batch. (2026-11-27)
- **A compatibility layer for existing software, later.** The kernel is
  designed from the start with the features such a layer needs, so they are
  not bolted on afterwards. (2026-11-27)

## Foundations

- **Rust** for the kernel and the core system. Memory safety removes the bug
  classes behind most serious operating system vulnerabilities. (2026-11-26)
- **Original code.** No Linux kernel code, and nothing copied from another
  operating system. Small, vetted libraries may be considered case by case;
  there are none today. (2026-11-26)
- **Apache License 2.0.** Permissive, with an explicit patent grant.
  (2026-11-27)

## Kernel

- **Microkernel with a small core that can be proved correct.** The core
  covers memory management, the permission model and messaging. Its small
  size is what makes machine-checked proofs feasible. (2026-11-27)
- **Every driver and service is a separate component behind a message
  interface.** A component can then be tested alone and moved behind a
  stronger wall without being rewritten. (2026-11-27)
- **The wall depends on trust.** Drivers and services we write ourselves in
  safe Rust may share memory with the core, which keeps them fast. Untrusted
  code runs in separate processes: applications, the compatibility layer, and
  anything that parses hostile input. (2026-11-27)
- **Memory management and page-fault handling stay in the kernel.** A
  production microkernel measured page faults several times slower with this
  work split out. (2026-11-27)
- **Bulk data moves through shared memory in batches; messages carry
  control.** This is what closes most of the speed gap with a monolithic
  kernel. (2026-11-27)
- **Speed is checked, not assumed.** Benchmarks for program start, small
  file operations, messaging and page faults run in the pull request checks
  once those features exist. (2026-11-27)

## Hardware

- **x86-64 only, in QEMU first, then one real PC.** Narrow hardware support
  keeps driver work bounded. (2026-11-26)
- **Our own drivers,** written from public specifications. Standard devices
  first: NVMe storage, USB, keyboard and mouse, the firmware's basic display.
  (2026-11-27)

## Verification

- **Layers of checking.** Unit tests on the development machine, a boot test
  in QEMU on every pull request, fuzzing of the system-call boundary, Miri
  for unsafe code, and machine-checked proofs for the core. (2026-11-26)
- **Nothing merges without passing the checks.** (2026-11-26)

## Process

- **Rigour through testing.** A change is trusted because a machine checked
  it, not because it was read. The layers are in [testing.md](testing.md).
  (2026-11-27)
- **Before-and-after snapshots for speed and security.** The system reports
  its state, the expected report is stored, and every unexplained difference
  fails. This covers memory, leaks, message counts, permissions and attack
  surface. (2026-11-27)
- **Corrections climb the trust ladder.** A mistake is prevented by code
  first, then by a check, and by a written rule only as a last resort.
  (2026-11-27)
- **An independent verifier.** A change is attacked by an agent that has not
  seen the author's reasoning before the owner sees it. (2026-11-27)
- **Names follow Conventional Commits.** Branches, commit subjects and pull
  request titles carry the type of change. (2026-11-27)
- **The owner approves by merging.** Nothing reaches `main` any other way.
  (2026-11-27)

## Boot

- **Our own UEFI boot stage, with no third-party bootloader.** The firmware
  loads a small Rust program that we write. It keeps the stack original and
  needs no external binaries. See [boot.md](boot.md). (2026-11-27)
