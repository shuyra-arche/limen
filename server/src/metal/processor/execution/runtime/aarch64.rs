//! The non-nesting EL1h IRQ prefix with the base FP/SIMD save shape.

use core::{
    arch::{asm, naked_asm},
    marker::PhantomData,
    mem::offset_of,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::metal::processor::{
    executor::{aarch64::native, local},
    treaty::{ProcessorId, Stack},
};

use super::{
    super::{
        profile::aarch64::base_simd,
        terminal::{Terminal, aarch64 as terminal},
    },
    InstallError,
};

const STACK_BYTES: usize = 16 * 1024;
const IRQ_SLOT: usize = 5;
const EL1H: usize = 0b0101;
const PSTATE_MODE: usize = 0b1_1111;
// When FEAT_NMI is absent, these SCTLR_EL1 fields are RES0.
const NMI_CONTROLS: u64 = (1 << 61) | (1 << 62);

#[cfg(any(target_feature = "sve", target_feature = "sme"))]
compile_error!("AArch64 runtime IRQ currently requires base FP/SIMD");

/// Permanent IRQ stack for one admitted Executor and its fixed local handler.
pub struct RuntimeTrap {
    stack: Stack<STACK_BYTES>,
    occupied: AtomicBool,
    irq: for<'frame> fn(Irq<'frame>),
}

impl RuntimeTrap {
    pub const fn new(irq: for<'frame> fn(Irq<'frame>)) -> Self {
        Self {
            stack: Stack::new(),
            occupied: AtomicBool::new(false),
            irq,
        }
    }

    /// Install one EL1h IRQ slot and preserve the original terminal vectors.
    ///
    /// # Safety
    ///
    /// The trusted caller owns SP_EL0, VBAR_EL1 and this PE's exception routing
    /// and save profile. The stack and both vector tables remain reserved and
    /// mapped for the lifetime of the Executor. Ordinary code remains at EL1h
    /// with base FP/SIMD access, FEAT_NMI disabled and the reserved SP_EL0
    /// untouched. The handler keeps exceptions masked, fits its stack, and
    /// holds the actual, pre-admitted reception state required to settle each
    /// enabled IRQ. Other sources retain the original terminal path.
    pub unsafe fn install(&'static self) -> Result<(), InstallError> {
        let Some(local) = local::ordinary_local() else {
            return Err(InstallError::Context);
        };
        if !native::masked_stack() || !profile_ready() {
            return Err(InstallError::Context);
        }
        let protection = unsafe { local.acquire_protection() };
        if !protection.is_outer() {
            return Err(InstallError::Context);
        }
        if local.runtime().is_some() {
            return Err(InstallError::Occupied);
        }
        if native::entry()
            != Some(native::Entry {
                vector: terminal::vectors as *const () as usize,
            })
        {
            return Err(InstallError::Context);
        }
        if self.occupied.swap(true, Ordering::AcqRel) {
            return Err(InstallError::Occupied);
        }

        let entry = self.ordinary_entry();
        // The old entry runs on SP_EL1 or its separate terminal stack.
        // The new vector can use the independent SP_EL0 before ordinary
        // IRQs are admitted. The unaffected slots still enter the old vector.
        unsafe {
            asm!(
                "msr SP_EL0, {stack}",
                "msr VBAR_EL1, {vector}",
                "isb",
                stack = in(reg) self.stack.top(),
                vector = in(reg) entry.vector,
                options(nostack),
            );
        }
        if native::entry() != Some(entry) || !self.ordinary_context() {
            Terminal::early().reject(format_args!("FAIL Limen runtime IRQ installation\n"));
        }
        local.publish_runtime(self);
        Ok(())
    }

    pub(in crate::metal::processor) fn ordinary_entry(&self) -> native::Entry {
        native::Entry {
            vector: vectors as *const () as usize,
        }
    }

    pub(in crate::metal::processor) fn ordinary_context(&self) -> bool {
        if !uses_sp_el1() {
            return false;
        }
        let stack: usize;
        unsafe { asm!("mrs {}, SP_EL0", out(reg) stack, options(nomem, nostack, preserves_flags)) };
        stack == self.stack.top()
    }

    pub(in crate::metal::processor) fn interrupt_context(&self, observed: native::Entry) -> bool {
        !uses_sp_el1() && observed == self.ordinary_entry()
    }

    pub(in crate::metal::processor) fn stack(&self) -> Range<usize> {
        self.stack.range()
    }
}

fn uses_sp_el1() -> bool {
    let select: usize;
    unsafe { asm!("mrs {}, SPSel", out(reg) select, options(nomem, nostack, preserves_flags)) };
    select == 1
}

fn profile_ready() -> bool {
    let (access, control): (usize, u64);
    unsafe {
        asm!("mrs {}, CPACR_EL1", "mrs {}, SCTLR_EL1", out(reg) access, out(reg) control,
             options(nomem, nostack, preserves_flags));
    }
    base_simd(access) && control & NMI_CONTROLS == 0
}

/// This layer of a real same-level IRQ. The handler cannot retain the frame.
pub struct Irq<'frame> {
    processor: ProcessorId,
    frame: PhantomData<&'frame mut *mut ()>,
}

impl<'frame> Irq<'frame> {
    fn new(_frame: &'frame mut Frame, processor: ProcessorId) -> Self {
        Self {
            processor,
            frame: PhantomData,
        }
    }

    pub fn processor(&self) -> ProcessorId {
        self.processor
    }
}

#[repr(C, align(16))]
struct Frame {
    registers: [usize; 31],
    stack: usize,
    pc: usize,
    state: usize,
    fpcr: u64,
    fpsr: u64,
    simd: [u128; 32],
}

extern "C" fn dispatch(frame: &mut Frame) -> bool {
    if frame.state & PSTATE_MODE != EL1H {
        return false;
    }
    let Some((processor, runtime)) = local::interrupt_local() else {
        return false;
    };
    (runtime.irq)(Irq::new(frame, processor));
    true
}

#[unsafe(naked)]
#[rustc_align(2048)]
unsafe extern "C" fn vectors() -> ! {
    naked_asm!(
        ".set slot, 0",
        ".rept 16",
        ".if slot == {irq}",
        "b {capture}",
        ".else",
        // Keep the original slot and let that vector read its syndrome.
        "mov x0, #slot",
        "b {terminal}",
        ".endif",
        ".balign 128",
        ".set slot, slot + 1",
        ".endr",
        irq = const IRQ_SLOT,
        capture = sym capture,
        terminal = sym terminal::accept,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn capture() -> ! {
    naked_asm!(
        // The hardware masks exceptions before the first instruction. Other
        // vector slots cannot reenter this frame. SP_EL1 is left untouched.
        "msr SPSel, #0",
        "sub sp, sp, #{size}",
        "stp x0, x1, [sp]",
        "mrs x0, ELR_EL1",
        "mrs x1, SPSR_EL1",
        "stp x0, x1, [sp, #{pc}]",
        "msr SPSel, #1",
        "mov x0, sp",
        "msr SPSel, #0",
        "str x0, [sp, #{stack}]",
        "stp x2, x3, [sp, #16]",
        "stp x4, x5, [sp, #32]",
        "stp x6, x7, [sp, #48]",
        "stp x8, x9, [sp, #64]",
        "stp x10, x11, [sp, #80]",
        "stp x12, x13, [sp, #96]",
        "stp x14, x15, [sp, #112]",
        "stp x16, x17, [sp, #128]",
        "stp x18, x19, [sp, #144]",
        "stp x20, x21, [sp, #160]",
        "stp x22, x23, [sp, #176]",
        "stp x24, x25, [sp, #192]",
        "stp x26, x27, [sp, #208]",
        "stp x28, x29, [sp, #224]",
        "str x30, [sp, #240]",
        "mrs x0, FPCR",
        "mrs x1, FPSR",
        "stp x0, x1, [sp, #{fpcr}]",
        "stp q0, q1, [sp, #{simd} + 0]",
        "stp q2, q3, [sp, #{simd} + 32]",
        "stp q4, q5, [sp, #{simd} + 64]",
        "stp q6, q7, [sp, #{simd} + 96]",
        "stp q8, q9, [sp, #{simd} + 128]",
        "stp q10, q11, [sp, #{simd} + 160]",
        "stp q12, q13, [sp, #{simd} + 192]",
        "stp q14, q15, [sp, #{simd} + 224]",
        "stp q16, q17, [sp, #{simd} + 256]",
        "stp q18, q19, [sp, #{simd} + 288]",
        "stp q20, q21, [sp, #{simd} + 320]",
        "stp q22, q23, [sp, #{simd} + 352]",
        "stp q24, q25, [sp, #{simd} + 384]",
        "stp q26, q27, [sp, #{simd} + 416]",
        "stp q28, q29, [sp, #{simd} + 448]",
        "stp q30, q31, [sp, #{simd} + 480]",
        "mov x0, sp",
        "bl {dispatch}",
        "cbz w0, 2f",
        "ldp q0, q1, [sp, #{simd} + 0]",
        "ldp q2, q3, [sp, #{simd} + 32]",
        "ldp q4, q5, [sp, #{simd} + 64]",
        "ldp q6, q7, [sp, #{simd} + 96]",
        "ldp q8, q9, [sp, #{simd} + 128]",
        "ldp q10, q11, [sp, #{simd} + 160]",
        "ldp q12, q13, [sp, #{simd} + 192]",
        "ldp q14, q15, [sp, #{simd} + 224]",
        "ldp q16, q17, [sp, #{simd} + 256]",
        "ldp q18, q19, [sp, #{simd} + 288]",
        "ldp q20, q21, [sp, #{simd} + 320]",
        "ldp q22, q23, [sp, #{simd} + 352]",
        "ldp q24, q25, [sp, #{simd} + 384]",
        "ldp q26, q27, [sp, #{simd} + 416]",
        "ldp q28, q29, [sp, #{simd} + 448]",
        "ldp q30, q31, [sp, #{simd} + 480]",
        "ldp x0, x1, [sp, #{fpcr}]",
        "msr FPCR, x0",
        "msr FPSR, x1",
        "ldp x0, x1, [sp, #{pc}]",
        "msr ELR_EL1, x0",
        "msr SPSR_EL1, x1",
        "ldp x0, x1, [sp, #0]",
        "ldp x2, x3, [sp, #16]",
        "ldp x4, x5, [sp, #32]",
        "ldp x6, x7, [sp, #48]",
        "ldp x8, x9, [sp, #64]",
        "ldp x10, x11, [sp, #80]",
        "ldp x12, x13, [sp, #96]",
        "ldp x14, x15, [sp, #112]",
        "ldp x16, x17, [sp, #128]",
        "ldp x18, x19, [sp, #144]",
        "ldp x20, x21, [sp, #160]",
        "ldp x22, x23, [sp, #176]",
        "ldp x24, x25, [sp, #192]",
        "ldp x26, x27, [sp, #208]",
        "ldp x28, x29, [sp, #224]",
        "ldr x30, [sp, #240]",
        "add sp, sp, #{size}",
        "eret",
        "2: add sp, sp, #{size}",
        "msr SPSel, #1",
        "mov x0, #{irq}",
        "b {terminal}",
        size = const size_of::<Frame>(),
        pc = const offset_of!(Frame, pc),
        stack = const offset_of!(Frame, stack),
        fpcr = const offset_of!(Frame, fpcr),
        simd = const offset_of!(Frame, simd),
        irq = const IRQ_SLOT,
        dispatch = sym dispatch,
        terminal = sym terminal::accept,
    );
}
