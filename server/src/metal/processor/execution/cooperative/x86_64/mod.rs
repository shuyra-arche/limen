//! Win64 cooperative call boundary, shared by the native x86_64 profiles.

use core::{
    arch::{asm, naked_asm, x86_64::__cpuid},
    mem::offset_of,
};

use crate::metal::processor::{
    description::x86_64::status,
    execution::profile::x86_64::{StateControl, base_xcr0},
};

// The base save shape does not own AVX or other extended components.
#[cfg(target_feature = "avx")]
compile_error!("cooperative x86_64 requires the base x87/SSE execution profile");

pub(in crate::metal::processor) fn ready() -> bool {
    let features = __cpuid(1);
    let (cr0, cr4, flags): (usize, usize, usize);
    unsafe {
        asm!("mov {}, cr0", "mov {}, cr4", "pushfq", "pop {}",
            out(reg) cr0, out(reg) cr4, out(reg) flags);
    }
    if !status::paging(cr0) || !status::interrupts_masked(flags) {
        return false;
    }
    match StateControl::from_registers(features.edx, features.ecx, cr0, cr4) {
        None => false,
        Some(StateControl::Legacy) => true,
        Some(StateControl::Xcr0) => {
            let (low, high): (u32, u32);
            unsafe {
                asm!("xgetbv", in("ecx") 0u32, out("eax") low, out("edx") high,
                    options(nomem, nostack, preserves_flags));
            }
            base_xcr0(u64::from(high) << 32 | u64::from(low))
        }
    }
}

#[repr(C, align(16))]
pub(in crate::metal::processor) struct Saved {
    resume: usize,
    stack: usize,
    preserved: [usize; 8],
    floating: [u8; 512],
}

impl Saved {
    pub(in crate::metal::processor) const EMPTY: Self = Self {
        resume: 0,
        stack: 0,
        preserved: [0; 8],
        floating: {
            let mut bytes = [0; 512];
            // Architectural initial x87 control word and MXCSR. Empty x87
            // tags and zero register bytes form a valid FXSAVE image.
            bytes[0] = 0x7f;
            bytes[1] = 0x03;
            bytes[24] = 0x80;
            bytes[25] = 0x1f;
            bytes
        },
    };

    pub(in crate::metal::processor) fn initial(
        stack: usize,
        entry: extern "C" fn(*const ()) -> !,
    ) -> Self {
        let mut saved = Self {
            resume: (first as *const ()).addr(),
            stack,
            ..Self::EMPTY
        };
        saved.preserved[4] = (entry as *const ()).addr();
        saved
    }
}

const _: () = assert!(offset_of!(Saved, floating).is_multiple_of(16));

/// The first entry has an aligned unused stack. Supply Win64 home space
/// before a real call; the Rust stub invokes the target's actual C ABI.
#[unsafe(naked)]
unsafe extern "win64" fn first() -> ! {
    naked_asm!(
        "sub rsp, 32",
        "mov rdx, r12",
        "call {launch}",
        "ud2",
        launch = sym launch,
    )
}

extern "win64" fn launch(argument: *const (), entry: extern "C" fn(*const ()) -> !) -> ! {
    entry(argument)
}

/// Save a normal call continuation (including the Win64 nonvolatile
/// integers and the complete base floating/SIMD environment). GS and the
/// interrupt configuration belong to the executing Hart, not the Unit.
#[unsafe(naked)]
pub(in crate::metal::processor) unsafe extern "win64" fn switch(
    outgoing: *mut Saved,
    incoming: *const Saved,
    argument: *const (),
) {
    naked_asm!(
        "mov rax, [rsp]",
        "mov [rcx + {resume}], rax",
        "lea rax, [rsp + 8]",
        "mov [rcx + {stack}], rax",
        "mov [rcx + {preserved} + 0], rbx",
        "mov [rcx + {preserved} + 8], rbp",
        "mov [rcx + {preserved} + 16], rdi",
        "mov [rcx + {preserved} + 24], rsi",
        "mov [rcx + {preserved} + 32], r12",
        "mov [rcx + {preserved} + 40], r13",
        "mov [rcx + {preserved} + 48], r14",
        "mov [rcx + {preserved} + 56], r15",
        "fxsave64 [rcx + {floating}]",
        "fxrstor64 [rdx + {floating}]",
        "mov rbx, [rdx + {preserved} + 0]",
        "mov rbp, [rdx + {preserved} + 8]",
        "mov rdi, [rdx + {preserved} + 16]",
        "mov rsi, [rdx + {preserved} + 24]",
        "mov r12, [rdx + {preserved} + 32]",
        "mov r13, [rdx + {preserved} + 40]",
        "mov r14, [rdx + {preserved} + 48]",
        "mov r15, [rdx + {preserved} + 56]",
        "mov rax, [rdx + {resume}]",
        "mov rsp, [rdx + {stack}]",
        "mov rcx, r8",
        "cld",
        "jmp rax",
        resume = const offset_of!(Saved, resume),
        stack = const offset_of!(Saved, stack),
        preserved = const offset_of!(Saved, preserved),
        floating = const offset_of!(Saved, floating),
    )
}

/// Enter the resident dispatcher with this target's existing kernel ABI.
#[unsafe(naked)]
pub(in crate::metal::processor) unsafe extern "win64" fn start_stack(
    stack: usize,
    entry: extern "C" fn(*const ()) -> !,
    argument: *const (),
) -> ! {
    core::arch::naked_asm!("mov rsp, rcx", "sub rsp, 32", "mov rcx, r8", "call {launch}", "ud2", launch = sym launch);
}
