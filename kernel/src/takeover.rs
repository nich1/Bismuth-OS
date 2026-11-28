//! Takes the processor over from the firmware: builds the kernel's own
//! segment table (GDT), task state segment (TSS) and interrupt table (IDT)
//! with `bismuth-descriptors`, loads them, checks the segment registers,
//! and turns on write protection for ring 0. Until this has run, a fault in
//! the kernel runs the firmware's handler; afterwards, the kernel's own
//! (`vectors`).
//!
//! The tables and the three emergency stacks are statics, so they have
//! fixed addresses in the kernel's writable segment and need no allocation.
//! Each is a [`HandedOver`]: written once, here, through a raw pointer, then
//! used only by the processor.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use bismuth_descriptors::{
    Error, GDT_ENTRIES, Gate, GateKind, InterruptStack, Privilege, PseudoDescriptor, Segment,
    TSS_INDEX, TSS_SELECTOR, TSS_SIZE, Tss, TssDescriptor,
};

use crate::vectors::{self, VECTORS};
use crate::{cpu, load, serial};

/// Memory written once, by [`take_over`], then handed to the processor.
///
/// `UnsafeCell` makes it writable memory, which the processor needs (it
/// marks the TSS descriptor busy, and pushes onto the stacks), and lets
/// [`take_over`] write it through a raw pointer. No reference to the
/// contents is ever made, so Rust's rules for references cannot be broken
/// by the processor's own reads and writes.
#[repr(transparent)]
struct HandedOver<T>(UnsafeCell<T>);

// SAFETY: only `take_over` writes the contents, and it does so at most once
// (`TAKEN`), on the only processor running, with interrupts off, before
// loading them. After that only the processor uses them. No Rust reference
// to the contents exists, so sharing the statics between threads, which
// the kernel does not have, could not race through Rust code either.
unsafe impl<T> Sync for HandedOver<T> {}

impl<T> HandedOver<T> {
    const fn new(value: T) -> Self {
        HandedOver(UnsafeCell::new(value))
    }

    fn pointer(&self) -> *mut T {
        self.0.get()
    }

    fn address(&self) -> u64 {
        self.pointer().addr() as u64
    }
}

/// The IDT: one 16-byte gate per vector, aligned as the manual advises.
#[repr(C, align(16))]
struct Idt([[u64; 2]; VECTORS]);

/// The TSS's 104 bytes, aligned so they never cross a page boundary.
#[repr(C, align(128))]
struct TssBytes([u8; TSS_SIZE]);

/// An emergency stack: 4 pages, plenty for the handler, which only prints.
#[repr(C, align(16))]
struct Stack([u8; STACK_BYTES]);
const STACK_BYTES: usize = 4 * 4096;

static GDT: HandedOver<[u64; GDT_ENTRIES]> = HandedOver::new([0; GDT_ENTRIES]);
static TSS: HandedOver<TssBytes> = HandedOver::new(TssBytes([0; TSS_SIZE]));
static IDT: HandedOver<Idt> = HandedOver::new(Idt([[0; 2]; VECTORS]));
static DOUBLE_FAULT_STACK: HandedOver<Stack> = HandedOver::new(Stack([0; STACK_BYTES]));
static NMI_STACK: HandedOver<Stack> = HandedOver::new(Stack([0; STACK_BYTES]));
static MACHINE_CHECK_STACK: HandedOver<Stack> = HandedOver::new(Stack([0; STACK_BYTES]));
/// Set by the first call of [`take_over`].
static TAKEN: AtomicBool = AtomicBool::new(false);

/// The vectors with a stack of their own, their interrupt-stack-table slot,
/// and the stack: the double fault (8), because it is how a fault on an
/// overflowed stack is reported, and the non-maskable interrupt (2) and
/// machine check (18), because they can arrive at any instruction, even one
/// that has just left the stack unusable.
const OWN_STACKS: [(usize, u8, &HandedOver<Stack>); 3] = [
    (8, 1, &DOUBLE_FAULT_STACK),
    (2, 2, &NMI_STACK),
    (18, 3, &MACHINE_CHECK_STACK),
];

const TABLES_OK: &str = "BISMUTH KERNEL TABLES OK\n";
const TABLES_ERROR: &str = "BISMUTH KERNEL TABLES ERROR: ";

/// Why the takeover stopped.
enum Failure {
    /// It had already run.
    Again,
    /// The library refused a value, which only a broken constant here does.
    Refused,
    /// A vector has no entry stub.
    NoStub,
    /// The segment registers and task register, read back, are not what
    /// was loaded: CS, DS, ES, SS, FS, GS, TR.
    Selectors([u16; 7]),
    /// `CR0.WP` reads clear after being set.
    WriteProtect,
}

impl From<Error> for Failure {
    fn from(_: Error) -> Failure {
        Failure::Refused
    }
}

/// Takes the processor over and prints `BISMUTH KERNEL TABLES OK`, or
/// prints what went wrong and stops through the kernel's failure path.
pub fn run() {
    let serial = serial::Com1::init();
    match take_over() {
        Ok(()) => serial.write_str(TABLES_OK),
        Err(failure) => {
            serial.write_str(TABLES_ERROR);
            match failure {
                Failure::Again => serial.write_str("already taken over"),
                Failure::Refused => serial.write_str("the descriptor library refused a value"),
                Failure::NoStub => serial.write_str("a vector has no entry stub"),
                Failure::WriteProtect => serial.write_str("CR0.WP did not stay set"),
                Failure::Selectors(found) => {
                    serial.write_str("selectors CS DS ES SS FS GS TR are");
                    for selector in found {
                        serial.write_str(" ");
                        serial.write_hex(u64::from(selector));
                    }
                }
            }
            serial.write_str("\n");
            crate::stop_failed();
        }
    }
}

fn take_over() -> Result<(), Failure> {
    if TAKEN.swap(true, Ordering::Relaxed) {
        return Err(Failure::Again);
    }
    let code = Segment::KernelCode.selector();
    let data = Segment::KernelData.selector();

    let mut tss = Tss::new();
    for (_, slot, stack) in OWN_STACKS {
        let top = stack.address() + STACK_BYTES as u64;
        tss.set_interrupt_stack(InterruptStack::new(slot)?, top)?;
    }
    // SAFETY: the first and only run (`TAKEN`), so nothing else uses the
    // TSS yet; the pointer is the static's own, aligned and valid.
    unsafe { TSS.pointer().write(TssBytes(*tss.as_bytes())) };

    let mut gdt = [0; GDT_ENTRIES];
    let tss_descriptor = TssDescriptor::new(TSS.address())?.encode();
    let segments = [
        Segment::Null,
        Segment::KernelCode,
        Segment::KernelData,
        Segment::UserData,
        Segment::UserCode,
    ];
    let entries = segments
        .map(|segment| (segment.index(), segment.encode()))
        .into_iter()
        .chain([
            (TSS_INDEX, tss_descriptor[0]),
            (TSS_INDEX + 1, tss_descriptor[1]),
        ]);
    for (index, entry) in entries {
        if let Some(slot) = gdt.get_mut(usize::from(index)) {
            *slot = entry;
        }
    }
    // SAFETY: as for the TSS; the GDT is not loaded yet.
    unsafe { GDT.pointer().write(gdt) };

    for vector in 0..VECTORS {
        let stack = OWN_STACKS
            .iter()
            .find(|(own, _, _)| *own == vector)
            .map(|&(_, slot, _)| InterruptStack::new(slot))
            .transpose()?;
        // Only breakpoint and overflow may be raised with `int3` and `into`
        // from ring 3; every other gate is for the processor and the kernel.
        let privilege = match vector {
            3 | 4 => Privilege::USER,
            _ => Privilege::KERNEL,
        };
        let handler = vectors::stub(vector).ok_or(Failure::NoStub)?;
        let gate = Gate::new(handler, code, stack, GateKind::Interrupt, privilege)?;
        // SAFETY: as for the TSS; the IDT is not loaded yet, and `vector`
        // is below `VECTORS`, so the gate is inside it.
        unsafe {
            IDT.pointer()
                .cast::<[u64; 2]>()
                .add(vector)
                .write(gate.encode())
        };
    }

    let gdt = PseudoDescriptor::gdt(GDT.address(), GDT_ENTRIES)?.to_bytes();
    let idt = PseudoDescriptor::idt(IDT.address(), VECTORS)?.to_bytes();
    // SAFETY: in this order, each step has what it needs. The GDT just
    // written holds the library's kernel code and data segments and an
    // available TSS descriptor, and like the TSS and IDT is a static, so it
    // stays in place. `ltr` needs the new GDT loaded; `lidt` comes last, so
    // that every gate's code selector and stack are in force when an
    // exception can first use them. Each gate leads to a stub in
    // `vectors`, which is safe to enter from any instruction because it
    // never returns.
    unsafe {
        load::load_gdt(&gdt);
        load::load_code_segment(code.value());
        load::load_data_segments(data.value());
        load::load_task_register(TSS_SELECTOR.value());
        load::load_idt(&idt);
    }
    let (code, data) = (code.value(), data.value());
    let expected = [code, data, data, data, 0, 0, TSS_SELECTOR.value()];
    let found = cpu::selectors();
    if found != expected {
        return Err(Failure::Selectors(found));
    }

    // SAFETY: only `WP` changes. The kernel writes none of its read-only
    // pages, and its writable ones are mapped writable, so setting it only
    // turns a write that should never happen into a page fault.
    unsafe { cpu::write_cr0(cpu::cr0() | cpu::CR0_WP) };
    if cpu::cr0() & cpu::CR0_WP == 0 {
        return Err(Failure::WriteProtect);
    }
    Ok(())
}
