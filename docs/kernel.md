# How the kernel takes over the processor

The boot stage jumps to the kernel with the firmware's tables still loaded
(`docs/boot.md`, "How the kernel is entered"). The first thing the kernel
does after checking its entry state is replace them with tables of its own,
so that from then on a fault in the kernel is reported by the kernel. This
explains what those tables are, how they are loaded, what happens when the
processor takes a fault, and what the fault scenarios in the boot test show.

## The three tables

An x86-64 processor finds three things through tables in memory that the
system sets up and points it at.

**The segment table** (the global descriptor table, GDT). On older x86
processors, every memory access went through a *segment*: a base address
and a size. In 64-bit mode segments are almost gone: bases and sizes are
ignored, and memory protection is done by the address tables instead. What
is left still matters, though. The code segment register, CS, names an
entry that says the code is 64-bit and which *privilege level* (ring) it
runs at: ring 0 for the kernel, ring 3 for programs. The stack segment, SS,
names one for the stack. A register holds a *selector*: an entry's index
times 8, plus the requested ring in the low bits. Bismuth's table, from the
`bismuth-descriptors` library, has entries for null (0x00, never used),
kernel code (0x08), kernel data (0x10), user data (0x1B) and user code
(0x23), in the order the fast system-call instructions will need, and the
task state segment's descriptor (0x28), which takes two entries.

**The task state segment** (TSS). The name is from 32-bit processors, which
could switch between tasks with it; 64-bit mode has no such switching. What
it holds now is stack pointers: the one to switch to when an interrupt
moves the processor from ring 3 to ring 0 (not used yet, since there is no
ring 3), and seven more, the *interrupt stack table*, that an interrupt
table entry can ask for by number. The processor finds the TSS through the
*task register*, loaded with `ltr` from the TSS's descriptor in the segment
table.

**The interrupt table** (the interrupt descriptor table, IDT). Each of the
256 *vectors* (numbers) has a *gate*: the address of the code to run, the
code segment to run it in, which interrupt stack to switch to (if any), and
whether interrupts are turned off on the way in. Vectors 0 to 31 are the
processor's *exceptions*: divide error is 0, invalid instruction 6, double
fault 8, general protection 13, page fault 14, and so on. The rest are for
devices and for software, which can raise one with `int n`. A gate also
says which rings may raise it with `int n`; for Bismuth only breakpoint (3)
and overflow (4) allow ring 3, since those exist for programs to use
(`int3`, `into`), and every other gate is for the processor and the kernel.
All 256 are *interrupt gates*, which turn interrupts off on entry.

## Why the kernel replaces the firmware's

The firmware's tables are the firmware's: they lie in memory the kernel
will not keep, their handlers are firmware code the kernel knows nothing
about, and they run on whatever stack was in use. In practice a fault in the
kernel printed the firmware's own report (a banner starting `!!!! X64
Exception Type`) and stopped, or, if the kernel's stack was what had gone
wrong, the processor could not even start that handler and reset the
machine without a word. The kernel also needs a TSS and user segments of
its own before it can ever run a program in ring 3.

## How the tables are loaded

`kernel/src/takeover.rs` builds the tables and `kernel/src/load.rs` holds
the instructions that load them. The order matters, because each step
needs what the one before it set up:

1. **Build.** The TSS gets the tops of the three emergency stacks (below),
   the segment table the library's entries and the TSS's descriptor, and
   the interrupt table one gate per vector, each leading to that vector's
   entry stub.
2. **`lgdt`** makes the new segment table current. Nothing changes yet:
   each segment register keeps a hidden copy of the entry it was loaded
   from, so the firmware's code segment is still in force.
3. **A far return** loads CS with the kernel's code selector. 64-bit mode
   has no instruction that loads CS directly, so the kernel pushes the
   selector and the address of the next instruction and "returns" to them
   with `retfq`.
4. **The data registers.** DS, ES and SS get the kernel's data selector.
   FS and GS get the null selector: nothing in the kernel uses them yet,
   null says so, and no register is left naming an entry of the
   firmware's table. Their bases are the one part of segmentation still
   used in 64-bit mode; a kernel that wants them sets them through
   model-specific registers, and must not count on a null load to clear
   them, since older AMD processors keep the old base.
5. **`ltr`** loads the task register from the TSS descriptor, which must be
   in the segment table just loaded. The processor marks that descriptor
   busy, a write, which is why the segment table is in writable memory.
6. **`lidt`** makes the new interrupt table current. It comes last so that
   every gate's code segment and stack are in force the moment an
   exception can first use them.
7. **Read back.** The kernel reads CS, DS, ES, SS, FS, GS and the task
   register and compares them with what it loaded. A missing far return
   would otherwise go unnoticed: everything keeps working on the firmware's
   code segment, since nothing in the kernel reloads CS.
8. **Write protection.** The kernel sets `CR0.WP` (below) and reads it back.

Then it prints `BISMUTH KERNEL TABLES OK`. If anything is wrong it prints
`BISMUTH KERNEL TABLES ERROR:` and the reason instead, for example the
selectors it read back, and stops with the kernel's failure code.

Between steps 2 and 6 the firmware's interrupt table is still in use but
its gates name the firmware's code selector (0x38 in QEMU), which is past
the end of the kernel's 7-entry segment table. A fault in that window, or
one with `lidt` left out, cannot be delivered at all: the processor raises
a general-protection fault trying, then a double fault, then resets
(QEMU's interrupt log shows exactly that sequence). Interrupts are off, so
only a fault in these few instructions, a non-maskable interrupt or a
machine check could arrive there.

**The memory.** The tables and the stacks are statics: they are part of the
kernel's image, at fixed addresses in its writable segment, and need no
allocation. Rust does not let a static be written unless it says how that
is safe, so each is wrapped in a small type, `HandedOver`, around
`UnsafeCell`, which marks memory that may change behind Rust's back and so
puts it in writable memory. `takeover` writes each one exactly once,
through a raw pointer, before loading it; a flag makes a second call an
error. No Rust reference to their contents is ever made, so the processor's
own writes (the busy bit, pushes onto the emergency stacks) cannot break
what a reference promises. After loading, only the processor uses them.

## What happens when the processor takes a fault

When an instruction faults, the processor stops it and, through the gate
for its vector:

1. switches to the gate's interrupt stack, if it names one;
2. aligns the stack pointer down to a multiple of 16;
3. pushes SS, the old stack pointer, RFLAGS, CS and the address of the
   faulting instruction (RIP), eight bytes each;
4. for some exceptions, pushes an *error code*;
5. turns interrupts off (an interrupt gate) and jumps to the handler.

The error code is there when the vector alone does not say enough. A
general-protection fault (13) can come from many causes, and its error code
names the segment or gate involved, if any; a page fault's (14) says
whether the page was there at all, whether it was a write, whether it was
an instruction fetch, and so on. The address that was touched is not in the
frame: the processor puts it in the CR2 register. A double fault always
pushes zero. The other exceptions push nothing, and the library's exception
table says which do.

So the stack looks different for different vectors. The kernel's entry
stubs (`kernel/src/vectors.rs`), one per vector, make it the same: each
pushes a zero where the processor pushed no error code, then its vector
number, and jumps to one common piece that calls the Rust handler with the
stack pointer, the address of this frame:

| Offset | What | Pushed by |
|---|---|---|
| 0 | vector number | the stub |
| 8 | error code, or 0 | the processor, or the stub |
| 16 | RIP | the processor |
| 24 | CS | the processor |
| 32 | RFLAGS | the processor |
| 40 | RSP | the processor |
| 48 | SS | the processor |

The stubs are assembled by a loop from a mask built at compile time from
the library's exception table, so which vectors get a zero cannot disagree
with the table; a unit test checks the mask against the table and against
the manual's list, and another checks the frame's offsets.

The handler prints one line and stops, with exit code 67 under `qemu-exit`:

`BISMUTH KERNEL FAULT vector=14 #PF error=0x3 rip=0xffffffff800050f7 rsp=0xffffffff7fffff28 cr2=0xffffffff80000a32`

The name is the manual's mnemonic, `reserved` for a reserved vector, the
manual's name for the two exceptions it gives no mnemonic (2 and 9), and
`interrupt` for 32 to 255. `cr2` is printed for page faults only. No
handler returns yet, so the stubs save no other registers and nothing runs
`iretq`, the instruction that would return.

## Why three exceptions get their own stacks

A gate without an interrupt stack runs its handler on the stack in use.
That is no good when the stack itself is the problem. When the kernel's
stack runs down into its guard page, the first write that touches the
guard page raises a page fault; but delivering the page fault means
pushing its frame
onto the same, overflowed stack, which faults again. A fault while
delivering a page fault is a *double fault*. If the double fault were also
delivered on that stack, it would fault a third time, and a third fault
makes the processor reset. So the double fault's gate names interrupt
stack 1, a stack of its own (16 KiB, a static), and it is reported instead.

The non-maskable interrupt (2) and the machine check (18) get stacks 2 and
3 for a related reason: they are not caused by the instruction running, so
they can arrive at any moment, including one at which the stack pointer
cannot be trusted.

## Write protection

Ring-0 code can write a read-only page unless `CR0.WP` is set. The firmware
decides it until the kernel runs (the boot stage's `PLACED` line reports it
as `wp=`), so the kernel sets it itself, as part of taking over the
processor: it is one bit in a register the kernel owns, and without it the
read-only segments of the kernel would not be read-only to the kernel.
The bit is read before (into the value written) and after; if it does not
stay set, the kernel stops with `BISMUTH KERNEL TABLES ERROR: CR0.WP did
not stay set`. In QEMU the firmware already leaves it set (`wp=1`), so the
read-only scenario below would pass there even without the kernel's
write; on a machine whose firmware leaves it clear, the write is what
makes the read-only pages refuse the kernel.

## The fault scenarios

The boot test builds one more kernel file per deliberate fault, each with
one of the kernel's `fault-*` features (`kernel/src/deliberate.rs`). Each
boots like the normal kernel up to `BISMUTH KERNEL TABLES OK`, then makes
its fault, and must print the kernel's own `FAULT` line, never `BISMUTH
KERNEL OK`, and exit with 67. The line's vector, name and error code must be
exact; RIP, RSP and CR2 must lie in the ranges given, worked out from the
same kernel file. "Code", "read-only" and "writable" are the kernel's
segments of those permissions; "stack" is the kernel's 64 KiB stack.

| Scenario | What the kernel does | Expected | RIP in | RSP in | CR2 in |
|---|---|---|---|---|---|
| `fault-divide` | divides by zero | `vector=0 #DE error=0x0` | code | stack | - |
| `fault-invalid-opcode` | runs `ud2` | `vector=6 #UD error=0x0` | code | stack | - |
| `fault-write-read-only` | writes a byte of its read-only segment | `vector=14 #PF error=0x3` | code | stack | read-only |
| `fault-run-data` | calls a byte of its writable segment | `vector=14 #PF error=0x11` | writable | stack | writable |
| `fault-guard-page` | reads the guard page below its stack | `vector=14 #PF error=0x0` | code | stack | guard page |
| `fault-stack-overflow` | calls itself until the stack runs out | `vector=8 #DF error=0x0` | not checked | guard page or the stack's lowest page | - |
| `fault-last-vector` | raises vector 255 with `int 255` | `vector=255 interrupt error=0x0` | code | stack | - |

**What they prove.** That the kernel's tables are in use, not the
firmware's: the line is the kernel's and no firmware banner appears. That
the stubs and the table agree on error codes, at least for the vectors
used: a wrong one shifts the frame, so the error code or RIP comes out
wrong. And they are the first evidence that the processor enforces the
permissions the boot stage mapped: a write to a read-only page is refused
(page-fault error 0x3: present, write), code in a writable page does not
run (0x11: present, instruction fetch; RIP and CR2 are the address
called), and the guard page is really missing (0x0: not present, read).
The overflow shows a fault on a broken stack is still reported, on the
double fault's own stack: it arrives as a double fault, not a page fault,
because the page fault cannot be delivered on the overflowed stack (its
RSP is in the guard page or just above it, and the manual leaves its RIP
undefined, so it is not checked). The last one shows the table covers all
256 vectors: with one entry fewer, `int 255` raises a general-protection
fault instead.

**What they do not prove.** Only these seven vectors are raised; the other
stubs are checked only through the shared mask and the table's test. The
non-maskable interrupt and machine check stacks are set up but never used.
Nothing returns from a handler, so nothing shows that a handler could. The
read-only scenario does not show that the kernel's own `CR0.WP` write
matters, since QEMU's firmware already sets it. And QEMU imitates the
processor; a real one could differ in details such as the saved RIP of a
double fault.

## Where the layouts and instructions come from

The tables' layouts are the library's (`descriptors/src/lib.rs`, which cites
its sources); the kernel works none of them out itself. The instructions
are in the Intel 64 and IA-32 Architectures Software Developer's Manual:
volume 2A, "LGDT/LIDT—Load Global/Interrupt Descriptor Table Register",
"LTR—Load Task Register", "LEA—Load Effective Address" and the entries
for `DIV` and `INT n`; volume 2B, "MOV—Move", "MOV—Move to/from Control
Registers", "RET—Return From Procedure", "STR—Store Task Register" and
the entries for `PUSH` and `UD2`. The frame, error codes and interrupt stacks are volume
3A, chapter "Interrupt and Exception Handling", sections "64-Bit Mode
Stack Frame", "Error Code", "Interrupt Stack Table" and "Exception and
Interrupt Reference" (its entries for the double fault and the page fault
give the error codes above). `CR0.WP` and CR2 are volume 3A, chapter
"System Architecture Overview", section "Control Registers".
