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

There is no kernel yet. The next step is for this stage to load one, collect
what the kernel needs to know about the machine (above all, the map of
usable memory), tell the firmware to step aside, and jump into the kernel.

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
