# The hand-off structure

This explains what the boot stage will tell the kernel when it starts it,
how that is laid out in memory, and why it is laid out this way. The code
is the `handoff` library (`bismuth-handoff`). Nothing uses it yet: a later
task has the boot stage fill it in and the kernel read it.

## What it is and why the kernel gets its own format

When the kernel starts, the firmware has gone. The kernel cannot ask it
anything, so whatever it needs to know about the machine has to be written
down for it beforehand. The most important thing is the *memory map*: which
ranges of physical memory exist and what each may be used for. Without it
the kernel cannot tell free memory from memory the firmware still needs, or
from a device's registers.

The firmware has a memory map of its own, but in the firmware's terms: a
list of *descriptors*, each a type number, a start address and a number of
4 KiB pages, with types such as "boot services data" that only make sense
while the firmware runs. The boot stage turns that into a list in Bismuth's
own terms, adds what only it knows (which pages it put the kernel in), and
hands the kernel that. The kernel then depends on nothing about how it was
loaded except this one structure, which is what `docs/decisions.md` asks
for: a different boot stage would only have to write the same structure.

## The layout

The structure is a run of bytes. Every field is at a fixed offset and is
stored little-endian (lowest byte first, as x86-64 stores numbers), so the
boot stage and the kernel agree on it byte for byte. That is a choice: Rust
lays out an ordinary type however the compiler likes, and the boot stage
and the kernel are built for different targets, so the library does not
rely on any type's layout. It writes and reads each field at its offset
with safe code, the way the ELF reader reads a file, and checks every
offset against the length of the bytes first.

**The header,** 24 bytes in version 1.0:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | Marker: the ASCII bytes `BISMHAND` |
| 8 | 2 | Major version: 1 |
| 10 | 2 | Minor version: 0 |
| 12 | 4 | Total length of the structure in bytes, header included |
| 16 | 4 | Header size in bytes: where the region list starts (24) |
| 20 | 4 | Number of regions |

**The region list** follows, one 24-byte entry per region, sorted by
address, no two overlapping:

| Offset in the entry | Size | Field |
|---|---|---|
| 0 | 8 | Physical start address, a multiple of 4096 |
| 8 | 8 | Length in 4 KiB pages, at least 1 |
| 16 | 4 | Kind, as a number (see below) |
| 20 | 4 | Written as zero, not read |

The last four bytes keep each entry a multiple of 8 bytes long.

**Versions.** A reader accepts only major version 1, and any minor version.
A minor version may only add what an older reader can skip without harm: a
longer header (the header size says where the regions start), fields after
the region list (the total length covers them), a meaning for an entry's
last four bytes, or new kind numbers, which an older reader treats as
reserved and so never uses. Anything else, such as moving a field, needs a
new major version, which an older reader refuses.

## The kinds of memory

| Number | Kind | What it means for the kernel |
|---|---|---|
| 1 | Usable | Free. The kernel may use it at once. |
| 2 | Kernel | The kernel's own: its segments, its stack and the frames of the address tables it starts on. Keep. |
| 3 | Boot services | Lent by the firmware for its boot services. Free once the kernel runs on address tables of its own: the first tables' lower levels are copies of the firmware's pointers into this memory. |
| 4 | Loader | The boot stage's own memory that is not the kernel's: its code, its data, the kernel file it read, and this structure. Free once the kernel runs on its own tables and has finished reading the structure. |
| 5 | Firmware runtime | The firmware's runtime services, which keep running after the boot stage leaves. Never touch. |
| 6 | ACPI reclaimable | Holds ACPI tables, which describe the machine. Free once the kernel has read them. |
| 7 | ACPI preserved | Firmware memory that must survive sleep states. Never touch. |
| 8 | Device | Registers of a device, reached at memory addresses. Not memory. |
| 9 | Persistent | Memory that keeps its contents without power. Not for general use. |
| 10 | Reserved | Unusable, reserved, or of a type Bismuth does not know. Never touch. |

Any other number is read as reserved. Number 0 is not used, so an entry
left as zeros is never mistaken for free memory.

The kernel's own memory is one kind rather than one each for segments,
stack and tables. The kernel can tell which frame is which by walking the
address tables it was started on, so separate kinds would only repeat that
and could disagree with it. If the kernel later needs them told apart, a
minor version can add kinds.

## From firmware types to kinds

The types come from the UEFI specification, version 2.10, section 7.2
"Memory Allocation Services": the `EFI_MEMORY_TYPE` list under
`AllocatePages()`, and the tables of how each type may be used before and
after `ExitBootServices()`. Every type number maps to exactly one kind, and
only conventional memory becomes usable.

| Type | Number | Kind | Why |
|---|---|---|---|
| `EfiReservedMemoryType` | 0 | Reserved | Not to be used |
| `EfiLoaderCode` | 1 | Loader | What the boot stage allocated |
| `EfiLoaderData` | 2 | Loader | What the boot stage allocated, including every page it puts the kernel in |
| `EfiBootServicesCode` | 3 | Boot services | The firmware's until the kernel leaves the tables that point here |
| `EfiBootServicesData` | 4 | Boot services | The same |
| `EfiRuntimeServicesCode` | 5 | Firmware runtime | The firmware keeps using it |
| `EfiRuntimeServicesData` | 6 | Firmware runtime | The same |
| `EfiConventionalMemory` | 7 | Usable | Free |
| `EfiUnusableMemory` | 8 | Reserved | The firmware found errors in it |
| `EfiACPIReclaimMemory` | 9 | ACPI reclaimable | ACPI tables, free once read |
| `EfiACPIMemoryNVS` | 10 | ACPI preserved | The firmware needs it kept across sleep |
| `EfiMemoryMappedIO` | 11 | Device | Device registers, not memory |
| `EfiMemoryMappedIOPortSpace` | 12 | Device | The same |
| `EfiPalCode` | 13 | Reserved | Processor firmware (on Itanium), to be kept |
| `EfiPersistentMemory` | 14 | Persistent | Keeps its contents, so not scratch memory |
| `EfiUnacceptedMemoryType` | 15 | Reserved | Faults until accepted, which Bismuth does not do |
| `EfiMaxMemoryType` and up | 16 to 0x6FFFFFFF | Reserved | Not valid types |
| Reserved for the machine's maker | 0x70000000 to 0x7FFFFFFF | Reserved | Meaning unknown to Bismuth |
| Reserved for operating system loaders | 0x80000000 to 0xFFFFFFFF | Reserved | Bismuth's boot stage uses none, so they are someone else's |

## Building it

The boot stage uses the `Builder`. It lends the builder space for a fixed
number of regions, adds the firmware's descriptors one at a time in any
order, then sets aside each range it used for the kernel, and finally has
the structure written into bytes it also provides. The builder never
allocates and never panics; every refusal leaves the map as it was.

- A descriptor with no pages describes nothing and is skipped.
- A start that is not a multiple of 4096, a range whose end does not fit in
  64 bits or passes 2^52 (the most physical memory x86-64 can address), and
  a descriptor that overlaps one already added are refused.
- A range set aside for the kernel must have at least one page and lie
  wholly in memory the firmware lists as the boot stage's (`EfiLoaderCode`
  or `EfiLoaderData`) and not already set aside. It may cover part of one
  region or several neighbouring ones. Memory the firmware lists as free is
  refused too: the boot stage gets every page it gives the kernel from the
  firmware, as loader data, so a free page there would mean a mistake.
- Running out of the lent space, and bytes too few for the structure, are
  refused.

When writing, the builder sorts the regions by address and merges
neighbours of the same kind, so the list is as short as it can be.

## Reading it

The kernel uses `Handoff::read`. It treats the bytes as untrusted: any
bytes give either an error or a view whose regions all lie inside them. It
checks the marker, the major version, that the total length fits the
bytes, that the header size is at least 24 and fits the length, that the
regions fit the length, and that each region starts on a page, has at least
one page, ends at or below 2^52 and starts at or after the end of the one
before. Then it offers the regions in order and the total number of usable
pages.

## How it is tested

`cargo xtask check` runs the library's unit tests (`handoff/src/tests.rs`).
The structures the reader is tested on are written byte by byte from the
tables above, not by the builder, so the two cannot share a mistake.

1. **One test per firmware type,** including both ends of each range of
   unassigned and set-aside numbers, and a test that no type other than 7
   is usable.
2. **Builder tests** for setting aside from the middle, the start and the
   end of a region and across several, merging, unsorted and overlapping
   input, every refusal, and space that is exactly full and one short.
3. **A randomised test** builds 20,000 random maps, compares the result of
   every step with a simple model (a table from page number to kind,
   including the error the model expects), then reads the written
   structure back and compares it with the model page by page.
4. **Reader tests** for each refusal, and a **corruption test** that
   damages a valid structure 50,000 times (random bytes, fields set to
   values at the edges, truncation) and checks that the reader never
   panics and never reports a region outside the bytes or out of order.
