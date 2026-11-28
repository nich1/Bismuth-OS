//! Processor instructions the kernel uses. The kernel's `unsafe` lives
//! here and in `port` and `serial`.

use core::arch::{asm, naked_asm};

/// Entry point. The linker's default entry symbol for this target is
/// `_start`, so the ELF file's entry address points here.
///
/// A naked stub, not a Rust function, because its job is to record the
/// stack pointer and flags exactly as the boot stage left them, and
/// compiled code may change the stack pointer before its first line runs.
/// It then puts the processor in the state the System V AMD64 psABI,
/// section 3.2 "Function Calling Sequence", promises a function on entry,
/// whatever it was given, and calls [`crate::kernel_main`] with the
/// hand-off structure's address and what it recorded, which checks them
/// against the entry contract. The boot stage leaves the address in `rdi`,
/// the first argument's register, which the stub does not touch:
///
/// 1. `mov rsi, rsp`: the stack pointer, the second argument.
/// 2. `pushfq`, `pop rdx`: `RFLAGS`, the third argument. This uses the
///    eight bytes below the stack pointer and leaves it as it was.
/// 3. `cld`: the compiler relies on the direction flag being clear.
/// 4. `and rsp, -16`, `call`: the call pushes its return address on a
///    16-byte boundary, so the stack pointer is 8 below one at
///    `kernel_main`'s entry, as the calling convention requires. With the
///    stack the contract gives, it moves down 16 bytes.
/// 5. `ud2`: `kernel_main` never returns; were it to, this faults.
///
/// The instructions are in the SDM, each under its own entry: `AND`,
/// `CALL` and `CLD` in volume 2A, `MOV`, `POP`, `PUSHFQ` and `UD2` in
/// volume 2B. `PUSHFQ` in ring 0 pushes `RFLAGS` as it is, interrupt and
/// direction flags included.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "sysv64" fn _start() -> ! {
    // SAFETY: entered by the boot stage's jump, with the stack the entry
    // contract gives (`docs/boot.md`): at least 16 writable bytes below
    // the stack pointer, which steps 2 and 4 use. Nothing else is read or
    // written, and `kernel_main` is an `extern "sysv64"` function taking
    // three `u64`s, in `rdi`, `rsi` and `rdx`, that never returns.
    naked_asm!(
        "mov rsi, rsp",
        "pushfq",
        "pop rdx",
        "cld",
        "and rsp, -16",
        "call {main}",
        "ud2",
        main = sym crate::kernel_main,
    )
}

/// Stops the processor for good, waking only to go back to sleep.
pub fn halt_forever() -> ! {
    loop {
        // SAFETY: `hlt` pauses the processor until the next interrupt. The
        // kernel runs in ring 0, where the instruction is allowed, and it
        // changes no memory or registers.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) }
    }
}
