# How Bismuth boots

This explains what happens between pressing the power button and Bismuth's
first line of output, and why it is built this way.

## The pieces

**Firmware** is the software built into the motherboard. On modern PCs it
follows a standard called UEFI. When the machine powers on, the firmware
starts the hardware, then looks on each disk for a program to run.

**The boot disk** needs one FAT-formatted partition. The firmware looks there
for a file at a fixed path, `EFI/BOOT/BOOTX64.EFI`, and runs it. That file is
an ordinary program in the same format Windows uses for `.exe` files.

**Bismuth's boot stage** is that file. It is the `boot` package in this
repository, written in Rust and built for the `x86_64-unknown-uefi` target,
which produces exactly the kind of program the firmware can load.

## What the boot stage does today

The firmware calls the function `efi_main` and passes it a pointer to the
*system table*: a structure of pointers to services the firmware offers, such
as printing text.

The boot stage then does three things:

1. **Writes `BISMUTH BOOT OK` to the serial port.** The serial port is an old,
   very simple device: send it a byte and the byte goes out on a wire. It
   needs no firmware help, which makes it the most dependable way to get text
   out of a machine this early.
2. **Prints a line on the firmware's console,** using the system table, so
   something also appears on the screen.
3. **Stops.** It puts the processor to sleep in a loop.

There is now a kernel file on the boot disk, but nothing loads it. The boot
stage does not yet read it, and it still prints its line and stops. The next
steps are for this stage to read the file from disk, then place it in memory,
collect what the kernel needs to know about the machine (above all, the map
of usable memory), tell the firmware to step aside, and jump into the kernel.

## The kernel file

The kernel is the `kernel` package, a separate program from the boot stage.
The boot stage is a firmware program: it is in the format the firmware knows
how to load, and it can call the firmware's services. The kernel runs with
nothing underneath it, no firmware and no operating system, so it is built for
a different Rust target, `x86_64-unknown-none` (x86-64, no operating system).
That target produces an ELF file. `cargo xtask build` copies it onto the boot
disk at `target/esp/EFI/BISMUTH/KERNEL.ELF`, next to the boot stage. Today the
kernel's entry function does one thing: it stops the processor in a loop.

**ELF** is the standard executable format on Linux and most other Unix-like
systems. A file starts with a *header* that says what kind of file it is, which
processor it is for, and the *entry point*: the address where execution
starts. The header points to a table of *segments*. Each segment is a piece of
the file that must be placed at a given address in memory, with permissions:
read, write, execute, or a mix. Loading an ELF file means copying each segment
into place and then jumping to the entry point. The kernel as built can be
placed at any address (it is *position-independent*) and has three loadable
segments: one read-only, one read and execute (the code), and one read and
write.

**The reader** is the `elf` library (`bismuth-elf`). Given the bytes of a
file, it checks the header and the segment table and returns the entry point
and the loadable segments, or an error that says what is wrong. It accepts only
64-bit x86-64 programs. It does not load anything, and it ignores sections,
symbols and relocations.

It is written defensively because it will run inside the boot stage, on a file
read from disk, before anything else protects the machine. A damaged or
malicious file must produce an error, never a crash and never a read outside
the file. So the library is safe Rust, uses no allocation, and has lints that
make indexing, unchecked arithmetic, `unwrap` and panics build failures. Every
size and offset in the file is checked before it is used, and each segment it
reports lies inside the file.

Tests cover this in three ways:

1. **Unit tests** build a small valid ELF file in memory and check that it is
   read correctly and that each kind of damage gets the right error.
2. **A corruption test** damages the valid file 50,000 times (random bytes,
   header fields set to values at the edges, truncation) and checks that the
   reader only ever returns an error or segments that lie inside the file.
3. **`cargo xtask check` reads the real kernel file** and fails unless it finds
   at least one loadable segment and an entry point inside an executable one.

## How it is tested

QEMU is a program that imitates a whole PC. `cargo xtask test` uses it:

1. It builds the boot stage and copies it to `target/esp/EFI/BOOT/BOOTX64.EFI`.
2. It starts QEMU with UEFI firmware and presents the `target/esp` folder as
   a FAT disk. No window is shown, and the imitation serial port is connected
   to the test.
3. The firmware finds and runs the boot stage, as it would on a real PC.
4. The boot stage prints its line. Because it was built with the `qemu-exit`
   feature, it then writes to a special QEMU device that makes QEMU quit with
   exit code 33.
5. The test passes if the serial output contains `BISMUTH BOOT OK` and QEMU
   exited with code 33 within two minutes. Anything else fails, and the
   captured output is shown.

The `qemu-exit` feature is off in normal builds. On a real PC nothing answers
at that port, so the write is left out entirely.

## Why our own boot stage

The usual choice for a new operating system is an existing bootloader such as
Limine or GRUB. Bismuth does not use one, for three reasons:

- **The stack stays original.** Booting is part of the system we set out to
  write.
- **No external binaries.** Everything that runs comes from this repository
  and the Rust compiler.
- **It is small.** UEFI already does the hard work of starting the hardware
  and reading the disk. A boot stage is a few hundred lines.

The cost is that we must write, ourselves, the code that loads the kernel and
hands over the machine's memory map. That is well specified and a good first
piece of real systems work. The choice is easy to reverse: the kernel will
not depend on how it was loaded beyond a small, documented hand-off
structure.

## Where the firmware layouts come from

The structures in `boot/src/uefi.rs` follow the UEFI specification: section 4
for the system table and section 12.4 for text output. Only the fields the
boot stage reads are declared.

The ELF layouts in `elf/src/lib.rs` follow the System V ABI's ELF
specification (chapters 4 and 5) and its x86-64 supplement, as the comments
there cite.
