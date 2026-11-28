//! Processor control registers and the one model-specific register the boot
//! stage reads and changes to give the kernel its addresses.
//!
//! The registers and bits come from the Intel 64 and IA-32 Architectures
//! Software Developer's Manual (SDM) volume 3A, chapter 2 "System
//! Architecture Overview": section "Control Registers" for CR0, CR3 and CR4,
//! and section "Extended Feature Enable Register" for `IA32_EFER`; CR3's
//! format under 4-level paging is in chapter 4 "Paging", section "4-Level
//! Paging and 5-Level Paging". The register's number is in SDM volume 4,
//! chapter 2 "Model-Specific Registers (MSRs)", table "IA-32 Architectural
//! MSRs". Whether the processor has no-execute pages is in SDM volume 2A,
//! chapter 3, the `CPUID` instruction's entry, leaf 80000001H. The AMD64
//! Architecture Programmer's Manual volume 2, chapter 3 "System Resources",
//! section "System-Control Registers" describes the same registers, and
//! its volume 3, appendix E, the same `CPUID` leaf.
//!
//! The moves to and from control registers leave the arithmetic flags
//! undefined (SDM volume 2B, the entry for `MOV` to and from control
//! registers), so their `asm!` blocks do not promise to keep the flags.
//! `rdmsr`, `wrmsr` and `cpuid` affect no flags.

use core::arch::asm;

/// `CR0.WP`, bit 16: when set, code in ring 0 cannot write to read-only
/// pages either.
pub const CR0_WP: u64 = 1 << 16;
/// `CR4.LA57`, bit 12: when set, the processor uses 5-level paging.
pub const CR4_LA57: u64 = 1 << 12;
/// The physical address of the top-level table in CR3: bits 51:12. The low
/// 12 bits hold caching flags, or the process-context identifier when
/// `CR4.PCIDE` is set.
pub const CR3_ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
/// `IA32_EFER.NXE`, bit 11: when set, the execute-disable bit (bit 63) of a
/// table entry forbids running code from the pages under it. While it is
/// clear, that bit is reserved and a set one makes the access fault.
pub const EFER_NXE: u64 = 1 << 11;
/// The number of the `IA32_EFER` model-specific register.
const IA32_EFER: u32 = 0xC000_0080;
/// The `CPUID` leaf that says which extended leaves exist, and the one
/// whose `EDX` bit 20 says whether the processor has no-execute pages
/// (Intel: "Execute Disable Bit available"; AMD: "NX").
const EXTENDED_LEAVES: u32 = 0x8000_0000;
const EXTENDED_FEATURES: u32 = 0x8000_0001;
const EDX_NO_EXECUTE: u32 = 1 << 20;

/// Reads CR0.
pub fn cr0() -> u64 {
    let value;
    // SAFETY: reading a control register changes nothing and is allowed in
    // ring 0, where UEFI programs run (as `halt` in `main.rs` relies on).
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack)) }
    value
}

/// Reads CR3: the physical address of the top-level address table in use,
/// and its flags.
pub fn cr3() -> u64 {
    let value;
    // SAFETY: as for `cr0`.
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack)) }
    value
}

/// Reads CR4.
pub fn cr4() -> u64 {
    let value;
    // SAFETY: as for `cr0`.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack)) }
    value
}

/// Reads `IA32_EFER`.
pub fn efer() -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: `rdmsr` of an existing register only reads it, in ring 0.
    // Every x86-64 processor has `IA32_EFER`: long mode, which UEFI x64
    // programs run in, is turned on through it.
    unsafe {
        asm!("rdmsr", in("ecx") IA32_EFER, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    u64::from(high) << 32 | u64::from(low)
}

/// Whether the processor can forbid running code from a page, so that
/// [`EFER_NXE`] may be turned on.
pub fn has_no_execute() -> bool {
    cpuid(EXTENDED_LEAVES).0 >= EXTENDED_FEATURES
        && cpuid(EXTENDED_FEATURES).1 & EDX_NO_EXECUTE != 0
}

/// `EAX` and `EDX` after `CPUID` with `EAX` = `leaf` and `ECX` = 0.
fn cpuid(leaf: u32) -> (u32, u32) {
    let (eax, edx);
    // SAFETY: `cpuid` only reads processor information into four
    // registers; every x86-64 processor has it, and leaves it does not
    // have return other values rather than faulting. It also writes
    // `ebx`, which LLVM reserves and an `asm!` block may not name as an
    // output, so `rbx` is saved in a free register and put back.
    unsafe {
        asm!(
            "mov {saved}, rbx",
            "cpuid",
            "mov rbx, {saved}",
            saved = out(reg) _,
            inout("eax") leaf => eax,
            inout("ecx") 0u32 => _,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    (eax, edx)
}

/// Writes `IA32_EFER`.
///
/// # Safety
///
/// `value` must be what [`efer`] returned with at most [`EFER_NXE`] turned
/// on, and that only if [`has_no_execute`] is true: setting a bit the
/// processor does not support faults. Changing any other bit can leave
/// long mode or fault. Turning `EFER_NXE` on is harmless only while no
/// table entry in use has bit 63 set, which holds while it is off, since
/// such an entry would fault.
pub unsafe fn set_efer(value: u64) {
    // SAFETY: `wrmsr` writes only the register named in `ecx`; the caller
    // vouches for the value and that the processor supports it.
    unsafe {
        asm!("wrmsr", in("ecx") IA32_EFER, in("eax") value as u32, in("edx") (value >> 32) as u32,
            options(nostack, preserves_flags));
    }
}

/// Turns interrupts off (`CLI`, SDM volume 2A, "CLI—Clear Interrupt
/// Flag"): once boot services have ended, the firmware's timer and its
/// interrupt handlers are no longer valid.
pub fn disable_interrupts() {
    // SAFETY: `cli` clears only `RFLAGS.IF`, and is allowed in ring 0
    // whatever the I/O privilege level. Not `nomem`: memory accesses that
    // an interrupt handler could see must not move across it.
    unsafe { asm!("cli", options(nostack)) }
}

/// Turns interrupts off, clears the direction flag, loads the stack pointer
/// with `stack_top - 8` and jumps to `entry`. Never returns.
///
/// The `- 8` makes the stack look as it does on entry to a function that a
/// `call` reached: System V x86-64 code expects the stack pointer plus 8 to
/// be a multiple of 16 there (System V AMD64 psABI, section 3.2 "Function
/// Calling Sequence"), the 8 being the return address `call` pushed. The
/// eight bytes at `stack_top - 8` stand where that return address would be.
///
/// The instructions are `CLI`, `CLD`, `LEA` and `JMP` (SDM volume 2A,
/// "CLI—Clear Interrupt Flag", "CLD—Clear Direction Flag", "LEA—Load
/// Effective Address" and "JMP—Jump"). `CLI` is allowed in ring 0 whatever the I/O
/// privilege level. Only the stack pointer, the instruction pointer and the
/// flags' interrupt and direction bits change; the general-purpose
/// registers keep whatever values they had, which the kernel may not rely
/// on, and everything else (privilege level, mode, segments, control
/// registers, the firmware's interrupt table) stays as it is, as the entry
/// contract in `docs/boot.md` lists.
///
/// # Safety
///
/// The address tables in CR3 must map `entry` as executable code that
/// expects to be entered this way and never returns, and must map the
/// whole stack below `stack_top` read and write, with `stack_top` a
/// multiple of 16. Nothing of this program's may be used again after the
/// jump: whatever it still borrows, the kernel may change.
pub unsafe fn enter_kernel(entry: u64, stack_top: u64) -> ! {
    // SAFETY: the caller vouches for the code at `entry` and the stack.
    // Nothing is pushed: the old stack is left as it is, and the new one is
    // the kernel's. `noreturn`: the jump does not come back, and the
    // kernel's entry never returns.
    unsafe {
        asm!(
            "cli",
            "cld",
            "lea rsp, [{top} - 8]",
            "jmp {entry}",
            top = in(reg) stack_top,
            entry = in(reg) entry,
            options(noreturn, nostack),
        )
    }
}

/// Writes CR3, which makes the processor use the top-level table at its
/// address and forget the translations it had cached (except global ones).
///
/// # Safety
///
/// `value` must be CR3's current flags with the address of a 4 KiB-aligned
/// top-level table below the processor's physical address width, built for
/// 4-level paging. Under it every address in use (this code, its stack and
/// data, the firmware's code and data while boot services run) must
/// translate as it does now, and the table and every table under it must
/// stay in place and unchanged, except through these rules, for as long as
/// the processor uses them.
pub unsafe fn set_cr3(value: u64) {
    // SAFETY: the caller vouches for the tables. Not `nomem`: every memory
    // access after this one goes through the new tables, so none may be
    // moved across it.
    unsafe { asm!("mov cr3, {}", in(reg) value, options(nostack)) }
}
