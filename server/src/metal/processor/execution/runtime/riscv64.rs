//! Permanent supervisor traps for the RISC-V integer execution profile.

use core::{
    arch::{asm, naked_asm},
    cell::Cell,
    marker::PhantomData,
    mem::offset_of,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::metal::{
    interrupt::riscv64::internal::supervisor,
    processor::{
        description::riscv64::status::{SIE, Status},
        execution::cooperative::Context,
        executor::{local, riscv64::native},
        treaty::ProcessorId,
    },
};

use super::{
    super::terminal::{Terminal, riscv64::trap as terminal},
    Capture, Entry, InstallError,
};

const STACK_BYTES: usize = 16 * 1024;
const SUPERVISOR_PREVIOUS: usize = 1 << 8;
const INTERRUPTS_PREVIOUS: usize = 1 << 5;
const INTERRUPT: usize = 1 << (usize::BITS - 1);
const SOFTWARE_CAUSE: usize = INTERRUPT | 1;
const TIMER_CAUSE: usize = INTERRUPT | 5;

/// One permanent, non-nesting supervisor trap stack and its fixed IRQ entry.
/// The entry may run only bounded processing without taking ordinary locks.
pub struct RuntimeTrap {
    stack: crate::metal::processor::treaty::Stack<STACK_BYTES>,
    occupied: AtomicBool,
    entry: Entry,
}

impl RuntimeTrap {
    pub const fn new(entry: Entry) -> Self {
        Self {
            stack: crate::metal::processor::treaty::Stack::new(),
            occupied: AtomicBool::new(false),
            entry,
        }
    }

    /// Install at the original local supervisor entry, with all IRQ lines masked.
    /// The previous terminal stack remains available for severe faults.
    ///
    /// # Safety
    /// This static's code and stack must remain mapped and reserved for the
    /// executor's lifetime. The profile must permit `sret`, retain the trusted
    /// supervisor `tp`, leave extension state Off, and never enter User mode.
    /// Enable only supervisor software and timer lines with live receivers.
    /// The handler must keep interrupts masked and settle the actual source
    /// or preserve its recheck obligation. A source without a configured
    /// handler takes the terminal path.
    pub unsafe fn install(&'static self) -> Result<(), InstallError> {
        let Some(local) = local::ordinary_local() else {
            return Err(InstallError::Context);
        };
        let status = native::status();
        if !status.masked()
            || !status.extensions_off()
            || !supervisor::all_masked()
            || !self.entry.software && !self.entry.timer
        {
            return Err(InstallError::Context);
        }
        let protection = unsafe { local.acquire_protection() };
        if !protection.is_outer() {
            return Err(InstallError::Context);
        }
        if local.runtime().is_some() || self.occupied.swap(true, Ordering::AcqRel) {
            return Err(InstallError::Occupied);
        }

        let entry = self.ordinary_entry();
        // The old vector uses its independent terminal stack without consulting
        // sscratch. After stvec changes, the new vector has its full stack.
        unsafe {
            asm!(
                "csrw sscratch, {stack}",
                "csrw stvec, {vector}",
                stack = in(reg) entry.scratch,
                vector = in(reg) entry.vector,
                options(nostack),
            );
        }
        if native::entry() != Some(entry) {
            // A partial control write cannot safely be retried or treated as an
            // ordinary rejection. Keep both storage owners and stop.
            Terminal::sbi_initial().reject(format_args!("FAIL Limen runtime trap installation"));
        }
        local.publish_runtime(self);
        Ok(())
    }

    pub(in crate::metal::processor) fn ordinary_entry(&self) -> native::Entry {
        native::Entry {
            vector: vector as *const () as usize,
            scratch: self.stack.top(),
        }
    }

    pub(in crate::metal::processor) fn ordinary_context(&self) -> bool {
        !self.stack().contains(&native::stack_pointer())
    }

    pub(in crate::metal::processor) fn covers_enabled(&self) -> bool {
        supervisor::enabled_covered(self.entry.software, self.entry.timer)
    }

    pub(in crate::metal::processor) fn interrupt_context(&self, observed: native::Entry) -> bool {
        observed.vector == self.ordinary_entry().vector && observed.scratch == 0
    }

    pub(in crate::metal::processor) fn stack(&self) -> Range<usize> {
        self.stack.range()
    }
}

/// This layer of an actual local supervisor timer interrupt.
pub struct TimerInterrupt<'frame> {
    processor: ProcessorId,
    instruction: usize,
    received: Cell<bool>,
    dispatcher: &'frame mut Option<&'static Context>,
}

impl<'frame> TimerInterrupt<'frame> {
    fn new(
        processor: ProcessorId,
        instruction: usize,
        dispatcher: &'frame mut Option<&'static Context>,
    ) -> Self {
        Self {
            processor,
            instruction,
            received: Cell::new(false),
            dispatcher,
        }
    }

    pub fn processor(&self) -> ProcessorId {
        self.processor
    }

    pub(in crate::metal) fn confirm_receipt(&self) {
        self.received.set(true);
    }

    pub(in crate::metal::processor) fn request_return(
        self,
        span: &Range<usize>,
        dispatcher: &'static Context,
    ) -> bool {
        if !self.received.get() || !span.contains(&self.instruction) {
            return false;
        }
        *self.dispatcher = Some(dispatcher);
        true
    }
}

/// This layer of an actual local supervisor software interrupt.
pub struct SoftwareInterrupt<'frame> {
    processor: ProcessorId,
    frame: PhantomData<&'frame mut *mut ()>,
}

impl<'frame> SoftwareInterrupt<'frame> {
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

// Indexed by RISC-V register number. x0 has no saved value.
#[repr(C)]
struct Frame {
    registers: [usize; 32],
    status: usize,
    pc: usize,
}

const _: () = assert!(size_of::<Frame>().is_multiple_of(16));

extern "C" fn dispatch(frame: &mut Frame, cause: usize) -> bool {
    if frame.status & SUPERVISOR_PREVIOUS == 0 || !Status::from_bits(frame.status).extensions_off()
    {
        return false;
    }
    let Some((processor, runtime)) = local::interrupt_local() else {
        return false;
    };
    let mut dispatcher = None;
    let capture = match cause {
        SOFTWARE_CAUSE if runtime.entry.software => {
            Capture::Software(SoftwareInterrupt::new(frame, processor))
        }
        TIMER_CAUSE if runtime.entry.timer => {
            Capture::Timer(TimerInterrupt::new(processor, frame.pc, &mut dispatcher))
        }
        _ => return false,
    };
    runtime.entry.handle(capture);
    if let Some(dispatcher) = dispatcher {
        // The linear request established an actual suspended local dispatcher.
        // The regular epilogue restores the trap stack before returning there.
        let continuation = unsafe { &*dispatcher.0.get() };
        frame.pc = continuation.return_address(&mut frame.registers);
        frame.status &= !(SIE | INTERRUPTS_PREVIOUS);
    }
    true
}

#[unsafe(naked)]
#[rustc_align(4)]
unsafe extern "C" fn vector() -> ! {
    naked_asm!(
        // Before stores can occur, sp names either the admitted mapped stack or
        // zero. The short swap/store prefix uses no recoverable instructions.
        "csrrw sp, sscratch, sp",
        "beqz sp, 4f",
        "addi sp, sp, -{size}",
        "sd t0, 5*8(sp)",
        "csrrw t0, sscratch, zero",
        "sd t0, 2*8(sp)",
        "sd zero, 0(sp)",
        // The status and PC are saved before dispatch. An unsupported trap
        // uses the independent terminal path and never returns through here.
        "sd x1, 1*8(sp)",
        "sd x3, 3*8(sp)",
        "sd x4, 4*8(sp)",
        "sd x6, 6*8(sp)",
        "sd x7, 7*8(sp)",
        "sd x8, 8*8(sp)",
        "sd x9, 9*8(sp)",
        "sd x10, 10*8(sp)",
        "sd x11, 11*8(sp)",
        "sd x12, 12*8(sp)",
        "sd x13, 13*8(sp)",
        "sd x14, 14*8(sp)",
        "sd x15, 15*8(sp)",
        "sd x16, 16*8(sp)",
        "sd x17, 17*8(sp)",
        "sd x18, 18*8(sp)",
        "sd x19, 19*8(sp)",
        "sd x20, 20*8(sp)",
        "sd x21, 21*8(sp)",
        "sd x22, 22*8(sp)",
        "sd x23, 23*8(sp)",
        "sd x24, 24*8(sp)",
        "sd x25, 25*8(sp)",
        "sd x26, 26*8(sp)",
        "sd x27, 27*8(sp)",
        "sd x28, 28*8(sp)",
        "sd x29, 29*8(sp)",
        "sd x30, 30*8(sp)",
        "sd x31, 31*8(sp)",
        "csrr t0, sstatus",
        "sd t0, {status}(sp)",
        "andi t0, t0, {supervisor}",
        "beqz t0, 3f",
        "csrr t0, sepc",
        "sd t0, {pc}(sp)",
        "csrr a1, scause",
        "bgez a1, 3f",
        "mv a0, sp",
        "call {dispatch}",
        "beqz a0, 3f",
        "ld t0, {pc}(sp)",
        "csrw sepc, t0",
        "ld t0, {status}(sp)",
        "csrw sstatus, t0",
        "ld x1, 1*8(sp)",
        "ld x3, 3*8(sp)",
        "ld x4, 4*8(sp)",
        "ld x6, 6*8(sp)",
        "ld x7, 7*8(sp)",
        "ld x8, 8*8(sp)",
        "ld x9, 9*8(sp)",
        "ld x10, 10*8(sp)",
        "ld x11, 11*8(sp)",
        "ld x12, 12*8(sp)",
        "ld x13, 13*8(sp)",
        "ld x14, 14*8(sp)",
        "ld x15, 15*8(sp)",
        "ld x16, 16*8(sp)",
        "ld x17, 17*8(sp)",
        "ld x18, 18*8(sp)",
        "ld x19, 19*8(sp)",
        "ld x20, 20*8(sp)",
        "ld x21, 21*8(sp)",
        "ld x22, 22*8(sp)",
        "ld x23, 23*8(sp)",
        "ld x24, 24*8(sp)",
        "ld x25, 25*8(sp)",
        "ld x26, 26*8(sp)",
        "ld x27, 27*8(sp)",
        "ld x28, 28*8(sp)",
        "ld x29, 29*8(sp)",
        "ld x30, 30*8(sp)",
        "ld x31, 31*8(sp)",
        "addi t0, sp, {size}",
        "csrw sscratch, t0",
        "ld x5, 5*8(sp)",
        "ld sp, 2*8(sp)",
        "sret",
        "3: ld sp, 2*8(sp)",
        "tail {terminal}",
        "4: csrrw sp, sscratch, zero",
        "tail {terminal}",
        size = const size_of::<Frame>(),
        status = const offset_of!(Frame, status),
        pc = const offset_of!(Frame, pc),
        supervisor = const SUPERVISOR_PREVIOUS,
        dispatch = sym dispatch,
        terminal = sym terminal,
    );
}
