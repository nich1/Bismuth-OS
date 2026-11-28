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

The boot stage then does eight things:

1. **Writes `BISMUTH BOOT OK` to the serial port.** The serial port is an old,
   very simple device: send it a byte and the byte goes out on a wire. It
   needs no firmware help, which makes it the most dependable way to get text
   out of a machine this early.
2. **Prints a line on the firmware's console,** using the system table, so
   something also appears on the screen.
3. **Reads the kernel file and checks it.** It reads
   `EFI\BISMUTH\KERNEL.ELF` from the disk it was started from, hands the bytes
   to the `elf` reader (described below) and writes one more line to the
   serial port: either `BISMUTH KERNEL FILE OK entry=0xffffffff80001190
   segments=2 bytes=5280` (the numbers are an example), or a line starting
   `BISMUTH KERNEL FILE ERROR:` with the reason. The reasons are: file not
   found, file is empty, file is too large (the limit is 64 MiB), reading
   failed, or the name of the error the ELF reader returned.
4. **Copies the kernel into memory.** It checks that the file can be placed
   at the addresses it was linked for (the rules are listed under "The kernel
   file" below), asks the firmware for whole 4 KiB pages for each loadable
   segment, clears them and copies the segment's bytes in, starting at the
   same place in the first page as its address. It records, per segment,
   where its pages are, the kernel address they belong at, how many there
   are and their permissions. Then it reads the pages back and writes
   `BISMUTH KERNEL COPIED segments=2 pages=2 sum=0x108120a9f6b1b5b1` (the
   numbers are an example; the sum changes when the kernel's source, its
   build settings or the compiler change, but not with the folder it is
   built in; it can differ between a Windows and a Linux build, whose panic
   messages write source paths with different separators, see "The kernel
   file" below): the sum is a 64-bit FNV-1a checksum of every byte of those
   pages. A broken rule, or the firmware refusing pages, gets a line
   starting `BISMUTH KERNEL PLACE ERROR:` instead.
5. **Gives the kernel its addresses and a stack.** It builds new address
   tables that keep everything the firmware had and add each segment at
   its kernel address, with its own permissions, the kernel's stack
   below them and, below the stack, the window the hand-off structure
   will be written in, read-only (see "How the kernel is entered" below),
   and switches the processor to them (see "Address tables" below). Then
   it checks the tables in use, reads
   every byte of the kernel back through its kernel addresses, and writes
   `BISMUTH KERNEL PLACED segments=2 pages=2 sum=0x108120a9f6b1b5b1
   perms=r--,r-x wp=1`: the sum must be the same as in the `COPIED` line,
   `perms` gives each segment's permissions as the tables have them (`r`
   for read, `w` for write, `x` for execute), and `wp` gives the state
   the firmware left `CR0.WP` in: whether read-only pages also refuse
   writes from the most privileged code, which the boot stage and the
   kernel run as, so the kernel's read-only pages included (the boot
   stage only reports it). Then it prints a line on the firmware's
   console, to show the firmware still works under the new tables.
   Anything that goes wrong gets a line starting `BISMUTH KERNEL MAP
   ERROR:` instead: 5-level paging is on, the processor cannot mark pages
   no-execute or did not turn that on, the top-level entry the kernel
   goes under is already used, the firmware has no memory for the tables,
   the stack or the hand-off structure, the table builder refuses a
   segment, the stack or the hand-off window, a page of a segment, of the
   stack or of the window is missing or has the wrong frame or
   permissions, the guard page below the stack is mapped, or the bytes
   read back differ.
6. **Leaves the firmware.** It reads the firmware's memory map and tells
   the firmware to step aside (see "Leaving the firmware" below), turns
   interrupts off, and writes `BISMUTH FIRMWARE LEFT descriptors=102
   usable=52425` (the numbers are an example): how many entries the map
   has, and how many 4 KiB pages of it are free for the kernel. The test
   build adds ` bs=0` (see below). If the
   firmware refuses, it writes a line starting `BISMUTH FIRMWARE EXIT
   ERROR:` instead: there is no memory map or exit service, asking for the
   map's size or reading the map failed, there is no memory for the map
   or for the hand-off's working space, the map no longer fits its
   buffer, its entries are smaller than the specification's, the exit
   failed, or the map changed twice in a row. The status is given where
   the firmware returned one.
7. **Builds the hand-off structure.** From the saved map it writes
   Bismuth's own memory map into the hand-off window, with the kernel's
   segments, its stack and its table frames marked as the kernel's (see
   "The hand-off" below and `docs/handoff.md`). If the hand-off library
   refuses, it writes a line starting `BISMUTH HANDOFF ERROR:` instead:
   a firmware descriptor (named by its start) is unaligned, ends past
   2^52 or overlaps another, a range of the kernel's is not wholly the
   boot stage's memory, the working space is full, or the structure
   needs more bytes than the window has.
8. **Jumps to the kernel.** It writes `BISMUTH KERNEL JUMP
   entry=0xffffffff800013a0 stack=0xffffffff80000000
   handoff=0xffffffff7ffe0000` on the serial port (the entry point is an
   example; the stack's top and the hand-off window are fixed), then
   turns interrupts off, moves to the kernel's stack, puts the hand-off
   address in `rdi` and jumps to the entry point. The kernel loads its
   own processor tables and prints `BISMUTH KERNEL TABLES OK`
   (`docs/kernel.md`), reads the structure, prints `BISMUTH KERNEL MEMORY
   regions=41 usable=52425 kernel=99` (the numbers are an example) and
   `BISMUTH KERNEL OK` on the serial port, and stops.

Each failure gets its own line, and then the boot stage stops, putting the
processor to sleep in a loop, because it is the only thing running. There
is no screen to rely on and nobody to report to, so a clear message on the
serial port is the only way to learn what went wrong.

The memory map reaches the kernel only through the hand-off structure;
nothing else is passed to it.

### Leaving the firmware

Until now the firmware owns the machine: its *boot services* hand out
memory, run its drivers and answer its timer's interrupts. Leaving it, by
the call `ExitBootServices`, ends all of that for good. From then on the
firmware does nothing unless asked through its few *runtime services*,
and the boot stage, then the kernel, is the only thing running. The
firmware's console, its disk and its memory allocation are gone with it.

The firmware only lets go if the boot stage shows that it knows the
current state of memory. The *memory map* is the firmware's list of every
range of physical memory and what it is used for: free, the firmware's
own, the boot stage's, holding ACPI tables, device registers, and so on.
Each version of the map has a *key*, and `ExitBootServices` takes the key
of the map last read. Any allocation changes the map and its key, so the
map must be read immediately before the call, after everything that
allocates. Then the map the kernel gets is the one in force when the
firmware stepped aside.

So the boot stage does this, once the kernel's pages, its tables, its
stack and the hand-off structure's pages are all allocated and the
firmware's console has printed its last line (the call takes the
console, so the compiler refuses a console call after it):

1. It asks the firmware how large the map is.
2. It allocates a buffer for the map (loader data, never freed), with
   room for eight entries more than asked for: allocating the buffer can
   itself split a free range into up to three, and the firmware may
   allocate for itself in between. The specification promises only the
   size with this answer, so the room is reckoned with the entry size the
   firmware states or the specification's 40 bytes, whichever is larger.
3. It allocates the hand-off builder's working space (loader data, whole
   pages, never freed): one region for each entry the buffer could hold
   at 40 bytes each, the smallest entry size the boot stage accepts, and
   two more for each of the ten ranges at most that it sets aside for
   the kernel (eight segments, the stack, the table frames), since one
   range can split a region in three and the builder counts its space
   before merging.
4. It reads the map into the buffer, and calls `ExitBootServices` with
   its key.
5. If the firmware answers that the key is stale, the map changed in
   between, so it reads the map again into the same buffer, without
   allocating, and tries once more. A second refusal, any other error, or
   a map that no longer fits is a failure. After a refused
   `ExitBootServices` only the firmware's memory services may be called,
   and the boot stage calls nothing but `GetMemoryMap`; the serial port
   and QEMU's exit device are written directly, not through the firmware.
6. It turns interrupts off at once: the firmware's timer and its
   handlers belonged to the boot services that have just ended. (Not
   before the call: if it fails, boot services keep running and expect
   interrupts as they left them. QEMU's firmware has already turned them
   off by the time the call returns.)

It counts the entries by stepping through the map by the entry size the
firmware states, which may be larger than the structure in the
specification (40 bytes; QEMU's firmware uses 48), and counts as usable
only the entries of type `EfiConventionalMemory`, through the same table
the hand-off structure uses (`docs/handoff.md`). An entry whose
attributes carry `EFI_MEMORY_SP` (memory set aside for a specific
purpose, section 7.2) is treated as `EfiReservedMemoryType` here and in
the hand-off structure, so it is never counted or reported as usable.

In the test build (`qemu-exit`), the boot stage reads the system table's
boot services pointer once the call has succeeded and adds it to the
`FIRMWARE LEFT` line as ` bs=<n>`; the boot test expects `bs=0`. QEMU's
firmware clears the pointer, and with the call left out it stays set, so
this shows the test that the call ran. The specification only says the
firmware should clear it, so this is evidence for the test, not a check
the boot stage makes on real hardware. Done by hand, a firmware call
made afterwards (printing on the firmware's console) also faults instead
of printing.

### The hand-off

Once boot services have ended, the boot stage builds the hand-off
structure (`docs/handoff.md`) with the `handoff` library, from the saved
map, in `boot/src/handoff.rs`: every firmware descriptor, stepping by the
entry size the firmware states, then the kernel's own ranges set aside:
each segment's pages, the stack's 16 pages and the whole block of 54
table frames. It writes the structure into the hand-off window's pages,
allocated before the map was read, and never writes them again. Any
refusal stops the boot with a `BISMUTH HANDOFF ERROR:` line. The test
build with the `damage-handoff` feature also damages the structure's
marker once it is written, for the boot test.

### How the kernel is entered

The kernel's entry point is entered by a jump, never a call. Much of the
state below is the firmware's, inherited unchanged: the UEFI
specification (version 2.10), section 2.3.4 "x64 Platforms", says what
the firmware sets up before it starts a program like the boot stage, and
the boot stage changes only what is listed here as its own. The kernel
relies on all of it, and must not rely on anything not listed.

- **Privilege and mode:** the most privileged level (ring 0), in 64-bit
  long mode, with the firmware's segment descriptor table and its code,
  stack and data segments still loaded. The boot stage changes none of
  them.
- **Paging:** on, with 4-level tables (`CR4.LA57` is off; the boot stage
  checks), `CR4.PAE` on as long mode requires, and `CR3` pointing at the
  tables the boot stage built (see "Address tables" below). Each kernel
  segment is at its linked address with its own permissions. The stack
  is 16 pages (64 KiB) at `0xFFFF_FFFF_7FFF_0000` up to
  `0xFFFF_FFFF_8000_0000`, writable, not executable, cleared; the page
  below it, at `0xFFFF_FFFF_7FFE_F000`, is not mapped, so running off the
  stack faults instead of overwriting other memory. The hand-off window
  is 8 pages (32 KiB) at `0xFFFF_FFFF_7FFE_0000` up to
  `0xFFFF_FFFF_7FFE_8000`, read-only, not executable; the pages between
  it and the guard page are not mapped. The firmware's
  mappings are all still there in the lower half, the boot stage's
  among them, but the kernel must not rely on them: they go once the
  kernel has its own tables.
- **Memory the kernel must not reuse yet:** the lower levels of the
  tables in CR3 are the firmware's own, shared by the copied top-level
  table, and they live in memory the firmware lent for its boot services
  (`EfiBootServicesData`). So does its interrupt table, and the handlers
  that table points to are in boot-services code (`EfiBootServicesCode`).
  Its segment descriptor table was found in runtime-services data under
  QEMU's firmware; another firmware may keep it elsewhere. Boot services
  have ended, so boot-services code and data are the system's to take
  back, but the processor still reads all of it.
  The kernel must not reuse it until it runs on tables of its own and has
  loaded descriptor and interrupt tables of its own. The kernel's
  segments, its stack, the frames of the new tables, the hand-off
  structure's pages, its working space and the memory map's buffer are
  loader data, which nothing else will take; the hand-off structure lists
  the first three as the kernel's and the rest as loader memory.
- **`IA32_EFER.NXE`:** on. The tables mark pages that may not run code
  with a bit that is reserved, and faults, while it is off. The boot
  stage turns it on if the firmware has not.
- **`CR0.WP`:** as the firmware left it, which the `PLACED` line reports.
  When it is clear, the kernel can write its own read-only pages.
- **Interrupts:** off (`RFLAGS.IF` clear), since just after boot services
  ended, and again in the jump. But the firmware's interrupt table is
  still loaded, so a non-maskable interrupt, a machine check or a fault
  in the kernel runs the firmware's handler, on the kernel's stack. That
  handler is in the lower half, which is still mapped, and in QEMU it
  still prints its report, but it belongs to the boot services that have
  ended. The kernel relies on that table and the firmware's segment
  table only until it has loaded its own, its first act after checking
  this state (`docs/kernel.md`).
- **Direction flag:** clear (`RFLAGS.DF`). The other bits of `RFLAGS` are
  undefined.
- **Stack pointer:** `0xFFFF_FFFF_7FFF_FFF8`, eight below the top. On entry
  to a function, System V x86-64 code expects the stack pointer plus 8 to
  be a multiple of 16, as it is just after a `call` has pushed a return
  address. The eight bytes at the stack pointer are zero.
- **`rdi`:** the hand-off structure's address, `0xFFFF_FFFF_7FFE_0000`,
  the start of the window. `rdi` is where a System V function takes its
  first argument (psABI section 3.2.3 "Parameter Passing"), so the entry
  stub leaves it alone and the kernel's Rust entry receives it as an
  ordinary argument. The structure is written in full, as
  `docs/handoff.md` lays it out, from the start of the window; the rest
  of the window is zero. Nothing writes the window after the jump, so
  the kernel may read it for as long as it runs on the boot stage's
  tables. Its pages are loader memory: once the kernel has its own
  tables, the window goes with them, so it must copy out what it needs
  first.
- **The other general-purpose registers are undefined.** The other
  control registers, and the floating-point and vector state, are as the
  firmware left them; the kernel is built not to use the floating-point
  and vector registers.
- **The firmware:** boot services have ended (`ExitBootServices`
  succeeded), so the firmware's console, disk and memory services are
  gone. The specification says the firmware should clear the system
  table's pointers to them, and QEMU's firmware does, but that is not
  guaranteed. Its runtime services remain, but nothing is passed to the
  kernel to find them with yet.
- **The serial port:** set to 115200 baud, 8 data bits, no parity, one
  stop bit by the boot stage, and written to by it and, until boot
  services ended, by the firmware.
  The kernel does not rely on that: it waits for the port to finish
  sending the boot stage's last bytes, then sets it up again the same way
  itself.

**What the kernel checks.** Its entry point is a short stub of a few
instructions (`kernel/src/cpu.rs`) that records the stack pointer (in
`rsi`) and `RFLAGS` (in `rdx`) before anything changes them, leaving
`rdi` as it is, then clears the direction flag, aligns the stack and
calls the kernel proper with the three. That checks, in
`kernel/src/entry.rs`, that interrupts were off, that the direction flag
was clear, and that the stack pointer was 8 below a multiple of 16 and
in the top half of the address range. If not, it prints `BISMUTH KERNEL
ENTRY ERROR:` and what is wrong (for example `interrupts are on`, or
`stack pointer not 8 below a multiple of 16: 0xffffffff80000000`) and
stops, with the kernel's failure code under `qemu-exit`. Then it loads
its own segment, task state segment and interrupt tables
(`docs/kernel.md`), and checks that the hand-off address is in the top half, on a page and the window's
address, makes the window into bytes (`kernel/src/handoff.rs`) and reads
the structure with the `handoff` library's reader. If either fails it
prints `BISMUTH KERNEL HANDOFF ERROR:` and what is wrong (for example
`the marker is wrong`) and stops the same way; otherwise it prints
`BISMUTH KERNEL MEMORY regions=<n> usable=<pages> kernel=<pages>`: the
number of regions, the reader's sum of usable pages, and the sum of the
pages of the kernel's own kind. The rest of the list it does not check:
a broken mapping or a missing `NXE` makes it fault instead, and the boot
stage's own checks come first.

The stack cannot meet a kernel segment: its top is the lowest address a
segment may start at, the placement rules refuse a segment below it or
one that runs past the top of the address range, and
`elf/src/place.rs` fails to build if the stack's constants are moved
above it. The hand-off window lies below the guard page, so below the
stack and every segment, and the same file fails to build if its
constants move it onto the guard page or out of the stack's 2 MiB. The
stack, its guard page and the window lie in the 2 MiB below the
kernel's 2 GiB, which no segment shares, so they need one directory and
one last-level table of their own. Before the jump the boot stage looks
up every page of the stack in the tables in use and requires its own
frame, writable and not executable, requires the guard page to be
missing, and requires every page of the window to lead to its own frame,
read-only and not executable.

### Address tables

A program never uses physical memory addresses directly. Every address it
uses, for code or data, is a *virtual* address, and the processor turns it
into a physical one by looking it up in *address tables* (usually called
page tables) that the system sets up. On x86-64 they are four levels of
tables, each a 4 KiB page of 512 entries: the top-level table's entry
covers 512 GiB, the next level's 1 GiB, then 2 MiB, and the last level's
entries each name one 4 KiB page of physical memory. An entry also says
what the page allows: writing, running code, or only reading. The
processor finds the top-level table through a register called CR3, and
switching address tables is one write to CR3.

While the firmware runs, its own tables map the memory in its memory map
(the memory it hands out and its own) at virtual addresses equal to the
physical ones (the UEFI specification requires this), and that is how the
boot stage has been reaching memory so far. The kernel, though, is
linked to run at `0xFFFF_FFFF_8000_0000`, an address the firmware maps to
nothing. So the boot stage builds new tables:

1. It copies the firmware's top-level table, all 512 entries, into a new
   one. The copy points to the firmware's lower tables, so everything the
   firmware mapped, including the boot stage itself and the firmware's own
   code, stays where it was. Without this, the very next instruction after
   the switch would be at an address that no longer exists. It copies
   rather than editing the firmware's table, because the firmware may have
   mapped its tables read-only and may still need them unchanged.
2. It adds the kernel under entry 511, the one covering the top 512 GiB,
   which the firmware leaves empty: each segment's kernel address leads to
   the pages the segment was copied into, with write and execute allowed
   only as the segment's flags say. These tables are built by the
   `paging` library (`bismuth-paging`). If entry 511 were already used,
   the boot stage stops, rather than share it with the firmware.
3. It loads the new top-level table into CR3.

Before mapping it checks that the processor uses 4-level paging, which is
all the library builds, and that it can mark pages no-execute, and turns
that feature on if the firmware has not, because the library marks pages
that may not run code with a bit the processor otherwise treats as an
error. The memory for
the tables comes from the firmware in one block, enough for any kernel the
placement rules accept, the stack and the hand-off window: 54 frames, of
which today's kernel, its stack and the window need six (the window
shares the stack's tables).

**What the `PLACED` line proves.** Each page of each segment is looked up
in the tables the processor is using (the address read back from CR3, so
a missing switch shows), by code separate from the code that built them,
and must lead to that segment's own frame with that segment's permissions.
Then every byte is read through the kernel addresses, so through the
processor's own lookup, and the sum must match the copy. Each page of the
stack is looked up the same way and must lead to its own frame, read and
write, and the guard page below it must not be mapped; each page of the
hand-off window must lead to its own frame, read-only. And the firmware
still prints on its console afterwards. That code at the kernel addresses
can run, on that stack, is shown next, by the kernel's own line.
**What it does not prove.** It does not show that read-only pages refuse
writes: the firmware decides that for now (`wp=1` means it does). It does not show the kernel is
ready to start: the firmware still owns the machine, and the tables still
hold all of its mappings, which the kernel will replace with its own. And
it looks only at the kernel's own pages, so it does not show that nothing
else is mapped in the kernel's half of the address range.

### How the file is read

The firmware offers its services as *protocols*: a named set of functions,
looked up by a 128-bit identifier. The boot stage asks the firmware for a
protocol by identifier and gets back a pointer to the functions. The lookup
itself, and requests for memory, go through the *boot services table*, which
the system table points to. Three protocols are used:

1. The *Loaded Image Protocol*, asked about the boot stage itself, says which
   disk it was loaded from. That is how it finds the right disk without
   guessing: the kernel file is on the same disk as the boot stage.
2. The *Simple File System Protocol* on that disk opens its top-level folder.
3. The *File Protocol* opens the kernel file by path, finds its size (by
   moving to the end and asking where it is), and reads it.

The boot stage has no memory of its own to put the file in: nothing has set up
a heap, and the firmware still owns the machine's memory. So it asks the
firmware to allocate a block of the right size and reads the file into that. It reads the
whole file before checking anything, because the ELF reader works on a
complete block of bytes and every offset it follows has to be checked against
the file's real length.

## The kernel file

The kernel is the `kernel` package, a separate program from the boot stage.
The boot stage is a firmware program: it is in the format the firmware knows
how to load, and it can call the firmware's services. The kernel runs with
nothing underneath it, no firmware and no operating system, so it is built for
a different Rust target, `x86_64-unknown-none` (x86-64, no operating system).
That target produces an ELF file. `cargo xtask build` copies a lean copy of
it onto the boot disk at `target/esp/EFI/BISMUTH/KERNEL.ELF`, next to the
boot stage. Today the
kernel's entry function checks its entry state, loads its own processor
tables and writes `BISMUTH KERNEL TABLES OK`, reads the hand-off structure
and writes its `MEMORY` line, then writes `BISMUTH KERNEL OK` and stops the
processor in a loop. If the kernel panics, it writes `BISMUTH KERNEL PANIC`
instead and stops; if it faults, it writes a `BISMUTH KERNEL FAULT` line
and stops (`docs/kernel.md`).

The file the linker makes is mostly debug information, which the boot stage
never reads. So `cargo xtask` writes two files from it next to the build,
under `target/x86_64-unknown-none/debug`: `bismuth-kernel.full`, with every
section, for a debugger, and `bismuth-kernel.lean`, with only the file
header, the program header table, the segments' bytes, the section header
table and the section names. The lean one goes on the boot disk, and on
every boot scenario's disk. The linker puts the file header inside the first
segment, so the header's offset of the section header table is loaded with
the kernel: both files therefore keep that table at the same offset, just
after the segments, and are the same bytes up to it. They load the same
pages, which `cargo xtask check` checks on the real files.

The kernel's bytes depend on its source, its build settings, the compiler
and, as the end of this paragraph explains, the operating system it is
built on, but not on the folder. The compiler records the folder it ran in, in the debug
information, so `cargo xtask` builds the kernel with
`--remap-path-prefix=<folder>=.`, which writes `.` instead. Paths in the
kernel's panic messages are already relative to the repository, or under
`/rustc/`, for the compiler's own library. `cargo xtask check` builds the
kernel in a copy of the repository in another folder and fails unless the
files are the same. A plain `cargo build` of the kernel, without
`cargo xtask`, still records the folder. Windows and Linux write those
relative paths with different separators, so the files may still differ
between the two.

**ELF** is the standard executable format on Linux and most other Unix-like
systems. A file starts with a *header* that says what kind of file it is, which
processor it is for, and the *entry point*: the address where execution
starts. The header points to a table of *segments*. Each segment is a piece of
the file that must be placed at a given address in memory, with permissions:
read, write, execute, or a mix. Loading an ELF file means copying each segment
into place and then jumping to the entry point.

The kernel is built to run at one fixed address, in the top 2 GiB of the
address range: its first byte goes at `0xFFFF_FFFF_8000_0000`. Its ELF file
is a *fixed-address executable*, so loading it needs no fixing up of
addresses. The settings that do this are in `.cargo/config.toml`. The kernel
has one loadable segment per kind of permission: one read-only, one read and
execute (the code), and one read and write for data it changes (today
only eight bytes, brought in by the Rust core library). Each segment starts on a page
of its own, so each can be given its own permissions.

**The reader** is the `elf` library (`bismuth-elf`). Given the bytes of a
file, it checks the header and the segment table and returns the entry point
and the loadable segments, or an error that says what is wrong. It accepts only
64-bit x86-64 programs. It does not load anything, and it ignores sections,
symbols and relocations.

It is written defensively because it runs inside the boot stage, on a file
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
   at least one loadable segment. It also runs the placement rules in
   `elf/src/place.rs`, the same ones the boot stage runs: the file is a
   fixed-address executable, its entry point and every segment lie at or
   above `0xFFFF_FFFF_8000_0000`, each segment sits at the same place in a
   page in memory as in the file, no two segments share a 4 KiB page,
   together they take at most 16384 pages (64 MiB, the same as the file
   limit) in at most 8 segments, no segment uses the last page of the
   address range (its end would be 2^64, which the boot stage cannot read
   back), and the entry point is inside a segment that may be run.

## How it is tested

QEMU is a program that imitates a whole PC. `cargo xtask test` uses it:

1. It builds the boot stage and the kernel, both with their `qemu-exit`
   features, and copies the boot stage to
   `target/esp/EFI/BOOT/BOOTX64.EFI`. It also builds a second kernel file
   with the kernel's `panic-at-entry` feature, which panics as soon as it
   has printed its `MEMORY` line, a second boot stage with the boot
   stage's `damage-handoff` feature, which damages the hand-off
   structure's marker after writing it, and one more kernel file for each
   of the kernel's seven `fault-*` features, which make one fault each
   once the `MEMORY` line is printed.
2. For each scenario below it lays out a separate disk folder, starts QEMU
   with UEFI firmware and presents that folder as a FAT disk. No window is
   shown, and the imitation serial port is connected to the test.
3. The firmware finds and runs the boot stage, as it would on a real PC.
4. The boot stage prints its lines. If anything failed, it then writes to
   a special QEMU device that makes QEMU quit with exit code 35. Otherwise
   it jumps to the kernel, which prints its own line and makes QEMU quit
   with codes of its own: 65 once it has printed `BISMUTH KERNEL OK`, 67
   if it panicked, refused its entry state or the hand-off structure, or
   reported a fault. (33 was the boot stage's success before it started the
   kernel; nothing uses it now.)
5. A scenario passes if the serial output contains `BISMUTH BOOT OK` and then
   its expected lines in order, and QEMU exits with the expected code within
   the time limit. Where a number can only be known within limits, the
   expected line gives a range, written `[40000..=60000]`, that the printed
   number must fall in. A range written `[usable:40000..=60000]` also
   names the number, and a later expected line's `[=usable]` must then be
   the same number: that is how the kernel's count is held to the boot
   stage's. Anything else fails, and the captured output is shown.
   If the firmware reports a processor exception (a line starting `!!!!
   X64 Exception Type`), for example because the kernel faulted, the test
   stops QEMU at once rather than wait for the time limit, since the
   firmware stops the processor after its report. A triple fault resets
   the machine, which QEMU's `-no-reboot` turns into exit code 0.

The twenty-one scenarios: the fourteen in the table below, then the
seven fault scenarios described after it. "Started" stands for the lines
of a boot that gets as far as the kernel: `BISMUTH KERNEL FILE OK
entry=... segments=... bytes=...`, matching what the ELF reader finds in the same file, then
`BISMUTH KERNEL COPIED segments=... pages=... sum=...`, matching the pages
the test lays out from the file itself, then `BISMUTH KERNEL PLACED` with
the same numbers and each segment's permissions from its flags (`wp=`
exactly `0` or `1`), then the firmware console's `Bismuth kernel addresses
checked.`, then `BISMUTH FIRMWARE LEFT descriptors=... usable=... bs=0`
with from 1 to 512 descriptors and from 40000 to 60000 usable pages (QEMU
is given 256 MiB, 65536 pages, of which the firmware and the boot stage
keep some; QEMU's firmware leaves about 52500 free; counting every type
as usable, or stepping through the map by the wrong entry size, falls far
outside) and the boot services pointer cleared, then `BISMUTH KERNEL JUMP
entry=... stack=0xffffffff80000000 handoff=0xffffffff7ffe0000` with the
file's entry point, then the kernel's `BISMUTH KERNEL TABLES OK`, then its
`BISMUTH KERNEL MEMORY regions=... usable=... kernel=...` with from 16 to 256 regions (QEMU's 102
descriptors merge to 41), exactly the usable pages of the `FIRMWARE LEFT`
line of the same boot (counted by the boot stage from the firmware's
map, and summed by the kernel from the structure the boot stage built),
and as the kernel's own exactly the file's segment pages as the test lays
them out, plus the stack's 16 pages and the 54 table frames. So the
usable memory the kernel reports is tied to what QEMU was given.

| Disk | Expected lines | Exit code |
|---|---|---|
| The real kernel file | Started, then `BISMUTH KERNEL OK` | 65 |
| No kernel file | `BISMUTH KERNEL FILE ERROR: file not found` | 35 |
| An empty file | `BISMUTH KERNEL FILE ERROR: file is empty` | 35 |
| A file whose signature is damaged | `BISMUTH KERNEL FILE ERROR: ELF reader: BadSignature` | 35 |
| A file cut off in the middle of the segment table | `BISMUTH KERNEL FILE ERROR: ELF reader: ProgramHeadersOutsideFile` | 35 |
| The kernel moved to a low address | `BISMUTH KERNEL PLACE ERROR: entry point below 0xffffffff80000000` | 35 |
| The second segment moved onto the first one's page | `BISMUTH KERNEL PLACE ERROR: segments 0 and 1 share a page` | 35 |
| The last segment grown to 64 MiB | `BISMUTH KERNEL PLACE ERROR: ... pages, more than the limit of 16384` | 35 |
| The last segment grown by two pages the file does not fill | Started, with the pages and sum the test lays out from this file, then `BISMUTH KERNEL OK` | 65 |
| The entry point moved to the start of the read-only segment | `BISMUTH KERNEL PLACE ERROR: entry point not in an executable segment` | 35 |
| The last segment moved onto the last page of the address range | `BISMUTH KERNEL PLACE ERROR: segment ... uses the last page of the address range`, naming the last segment | 35 |
| The real kernel file, on a processor without no-execute pages (QEMU's `-cpu qemu64,-nx`) | The `FILE OK` and `COPIED` lines, then `BISMUTH KERNEL MAP ERROR: the processor cannot mark pages no-execute` | 35 |
| The kernel file built to panic once it has read the hand-off | Started, then `BISMUTH KERNEL PANIC`, and never `BISMUTH KERNEL OK` | 67 |
| The real kernel file, started by the boot stage built to damage the hand-off structure's marker | Started up to the `JUMP` line and the kernel's tables line, then `BISMUTH KERNEL HANDOFF ERROR: the marker is wrong`, and never `BISMUTH KERNEL OK` | 67 |

Every scenario that reaches the kernel also expects `BISMUTH KERNEL TABLES
OK`, which the kernel prints once it has loaded its own tables, right after
the `JUMP` line and before it reads the hand-off structure. Seven more
scenarios, one per deliberate fault, follow these: each is started up to
the `MEMORY` line, then expects the kernel's own `FAULT` line; they are
described in `docs/kernel.md`.

The `qemu-exit`, `damage-handoff`, `panic-at-entry` and `fault-*` features
are off in normal builds. On a real PC nothing
answers at that port, so the writes are left out entirely.

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

The structures in `boot/src/uefi.rs` and the files beside it follow the UEFI
specification (version 2.10), declared by hand, and their comments name the
sections: 2.3.4 for the identity mapping on x64, 4.3 for the system table
(and its boot services pointer, read again after the exit in the test
build), 4.4 for the boot services table, 7.2 for memory and page allocation and
for `GetMemoryMap`, the memory descriptor and its attribute bits
(`EFI_MEMORY_SP`), 7.3 for looking up a
protocol, 7.4 for `ExitBootServices`, 9.1 for the Loaded Image
Protocol, 12.4 for text output, 13.4 for the Simple File System Protocol and
13.5 for the File Protocol; the status values are in appendix D. Only the
fields the boot stage uses are declared. The order of the calls that end
boot services, and the reading and count of the map's descriptors, are in
`boot/src/leave.rs`, and the building of the hand-off structure in
`boot/src/handoff.rs`; neither has firmware code.

The ELF layouts in `elf/src/lib.rs` follow the System V ABI's ELF
specification (chapters 4 and 5) and its x86-64 supplement, as the comments
there cite.

The address table layouts in `paging/src/lib.rs` follow the Intel 64 and
IA-32 Architectures Software Developer's Manual, volume 3A, chapter 4
"Paging" (sections "4-Level Paging and 5-Level Paging" and "Access
Rights"), and the AMD64 Architecture Programmer's Manual, volume 2,
chapter 5. The registers in `boot/src/cpu.rs` (CR0, CR3, CR4 and
`IA32_EFER`) follow the same Intel manual, volume 3A, chapter 2, sections
"Control Registers" and "Extended Feature Enable Register", and the AMD
manual, volume 2, chapter 3, section "System-Control Registers"; the
no-execute check reads `CPUID` leaf 80000001H (Intel volume 2A, the
`CPUID` entry; AMD volume 3, appendix E).

Interrupts are turned off after leaving the firmware with `CLI` (Intel
volume 2A). The jump to the kernel in `boot/src/cpu.rs` uses `CLI`, `CLD`, `LEA` and
`JMP` (Intel volume 2A, each instruction's entry), and the state it leaves
the stack and flags in is what the System V AMD64 psABI, section 3.2
"Function Calling Sequence", asks of a function's entry; the hand-off
address goes in `rdi`, a function's first argument under section 3.2.3
"Parameter Passing". The kernel's
entry stub in `kernel/src/cpu.rs` uses `MOV`, `PUSHFQ`, `POP`, `CLD`,
`AND`, `CALL` and `UD2` (Intel volumes 2A and 2B), and the flags it
checks in `kernel/src/entry.rs` are those of Intel volume 1, section
"EFLAGS Register". The kernel's
port access in `kernel/src/port.rs` uses `IN` and `OUT` (Intel volumes 2A
and 2B), and its serial port in `kernel/src/serial.rs` follows the
registers of the 16550 UART (the PC16550D data sheet).
