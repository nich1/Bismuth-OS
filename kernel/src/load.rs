//! The instructions that hand the processor new descriptor tables. Each
//! takes what `bismuth-descriptors` built (a pseudo-descriptor's bytes or
//! a selector's value), so no layout is worked out here. `takeover` calls
//! them in the order the next one needs.
//!
//! None of `LGDT`, `LIDT`, `LTR`, `MOV` to a segment register, `PUSH`,
//! `LEA` or a far `RET` changes a flag (SDM vol. 2A, "LGDT/LIDT—Load
//! Global/Interrupt Descriptor Table Register", "LTR—Load Task Register",
//! "LEA—Load Effective Address"; vol. 2B, "MOV—Move", "RET—Return From
//! Procedure", and the entry for `PUSH`).

use core::arch::asm;

/// `lgdt`: makes the table that `operand` (the 10 bytes of a
/// pseudo-descriptor) describes the segment table. The segment registers
/// keep the descriptors they hold until each is loaded again.
///
/// # Safety
///
/// The table must hold valid descriptors for every selector that will be
/// loaded from it, and stay at that address, unchanged except by the
/// processor, for as long as it is the segment table.
pub unsafe fn load_gdt(operand: &[u8; 10]) {
    // SAFETY: the caller vouches for the table; the instruction only reads
    // the 10 bytes of `operand`.
    unsafe {
        asm!("lgdt [{}]", in(reg) operand.as_ptr(),
            options(readonly, nostack, preserves_flags));
    }
}

/// Loads CS with `selector`. 64-bit mode has no `MOV` to CS, so this pushes
/// the selector and the address of the next instruction and makes a far
/// return to them, which loads both (SDM vol. 2B, "RET—Return From
/// Procedure", far return to the same privilege level).
///
/// # Safety
///
/// `selector` must name a present 64-bit code segment of the current
/// privilege level in the loaded segment table.
pub unsafe fn load_code_segment(selector: u16) {
    // SAFETY: the caller vouches for the selector; the two pushes are
    // popped again by `retfq`, which goes on at the next instruction.
    unsafe {
        asm!(
            "push {selector}",
            "lea {next}, [rip + 2f]",
            "push {next}",
            "retfq",
            "2:",
            selector = in(reg) u64::from(selector),
            next = lateout(reg) _,
            options(preserves_flags),
        );
    }
}

/// Loads DS, ES and SS with `data`, and FS and GS with the null selector
/// (SDM vol. 2B, "MOV—Move", which allows a null selector in DS, ES, FS
/// and GS).
///
/// # Safety
///
/// `data` must name a present, writable data segment whose privilege level
/// is the current one, in the loaded segment table.
pub unsafe fn load_data_segments(data: u16) {
    // SAFETY: the caller vouches for `data`, and the null selector needs no
    // descriptor. The stack pointer is unchanged, and in 64-bit mode the
    // segments' bases and limits are not used for these registers.
    unsafe {
        asm!(
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            "mov fs, {null:x}",
            "mov gs, {null:x}",
            data = in(reg) data,
            null = in(reg) 0u16,
            options(nostack, preserves_flags),
        );
    }
}

/// `ltr`: loads the task register with `selector`, which the processor
/// reads the TSS's address from (SDM vol. 2A, "LTR—Load Task Register").
/// The processor marks the descriptor busy, a write to the segment table.
///
/// # Safety
///
/// `selector` must name an available TSS descriptor in the loaded segment
/// table, for a TSS that stays in place for as long as it is loaded and
/// whose stack pointers lead to stacks nothing else uses.
pub unsafe fn load_task_register(selector: u16) {
    // SAFETY: the caller vouches for the selector and the TSS. Not
    // `readonly`: the instruction writes the descriptor's busy bit.
    unsafe {
        asm!("ltr {:x}", in(reg) selector, options(nostack, preserves_flags));
    }
}

/// `lidt`: makes the table that `operand` describes the interrupt table.
///
/// # Safety
///
/// Every gate in the table must lead to a handler that is safe to enter
/// from any instruction, and the table must stay at that address, unchanged,
/// for as long as it is the interrupt table.
pub unsafe fn load_idt(operand: &[u8; 10]) {
    // SAFETY: the caller vouches for the table; the instruction only reads
    // the 10 bytes of `operand`.
    unsafe {
        asm!("lidt [{}]", in(reg) operand.as_ptr(),
            options(readonly, nostack, preserves_flags));
    }
}
