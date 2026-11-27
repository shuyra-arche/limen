//! Same-level x86_64 interrupt entry with independently supplied IST stacks.

mod tables;

use core::{
    arch::{
        asm, naked_asm,
        x86_64::{__cpuid, __cpuid_count},
    },
    marker::PhantomData,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::metal::processor::{
    description::x86_64::status,
    executor::{local, x86_64::native},
    treaty::ProcessorId,
};

use super::{
    super::{
        profile::x86_64::{StateControl, base_xcr0},
        terminal::{Terminal, x86_64 as terminal},
    },
    InstallError,
};
use tables::Tables;

const FIRST_EXTERNAL: u8 = 32;
const CET: usize = 1 << 23;
const NESTED_TASK: usize = 1 << 14;
const VIRTUAL_8086: usize = 1 << 17;
const FXSAVE_BYTES: usize = 512;
static INITIAL_MXCSR: u32 = 0x1f80;

#[cfg(target_feature = "avx")]
compile_error!("x86_64 runtime IRQ currently requires the base x87/SSE profile");

/// Permanent per-Executor tables, independent stacks and a fixed IRQ handler.
pub struct RuntimeTrap {
    tables: Tables,
    occupied: AtomicBool,
    vector: u8,
    irq: for<'frame> fn(Irq<'frame>),
}

impl RuntimeTrap {
    pub const fn new(vector: u8, irq: for<'frame> fn(Irq<'frame>)) -> Self {
        Self {
            tables: Tables::new(),
            occupied: AtomicBool::new(false),
            vector,
            irq,
        }
    }

    /// Replace the early IDT after establishing the TSS and independent stacks.
    ///
    /// # Safety
    ///
    /// The caller owns the local controls and original entry, including the
    /// source GDT whose mapping continues for native launch. The new code and
    /// material must remain mapped and resident. Preserve CPL0, base x87/SSE
    /// state, CET disabled and the trusted GS base. Begin masked. Before
    /// enabling delivery, provision exclusive reception for the real selected
    /// source. The handler keeps ordinary interrupts masked, fits the IRQ
    /// stack and settles or preserves the original obligations before return.
    pub unsafe fn install(&'static self) -> Result<(), InstallError> {
        let local = local::ordinary_local().ok_or(InstallError::Context)?;
        if local.runtime().is_some() {
            return Err(InstallError::Occupied);
        }
        let early = terminal::idt();
        let expected = native::Entry {
            base: early.base,
            limit: early.limit,
        };
        if self.vector < FIRST_EXTERNAL || native::entry() != Some(expected) || !profile_ready() {
            return Err(InstallError::Context);
        }
        let protection = unsafe { local.acquire_protection() };
        if !protection.is_outer() {
            return Err(InstallError::Context);
        }
        if self.occupied.swap(true, Ordering::AcqRel) {
            return Err(InstallError::Occupied);
        }
        let result = unsafe {
            self.tables.install(
                self.vector,
                vector as *const () as usize,
                terminal::vector_address,
            )
        };
        if let Err(error) = result {
            // The table owner returns ordinary failure only before lgdt/ltr.
            self.occupied.store(false, Ordering::Release);
            return Err(error);
        }
        if native::entry() != Some(self.ordinary_entry()) || !self.ordinary_context() {
            Terminal::early().reject(format_args!("FAIL Limen runtime IRQ installation\n"));
        }
        local.publish_runtime(self);
        Ok(())
    }

    pub(in crate::metal::processor) fn ordinary_entry(&self) -> native::Entry {
        self.tables.entry()
    }

    pub(in crate::metal::processor) fn ordinary_context(&self) -> bool {
        self.tables.matches() && !self.tables.on_exception_stack(native::stack_pointer())
    }

    pub(in crate::metal::processor) fn interrupt_context(&self, observed: native::Entry) -> bool {
        observed == self.ordinary_entry() && self.tables.matches() && masked()
    }

    pub(in crate::metal::processor) fn stack(&self) -> Range<usize> {
        self.tables.stack()
    }

    pub(in crate::metal::processor) fn original_launch(&self) -> Option<(native::Descriptor, u16)> {
        self.tables.original_launch()
    }
}

fn masked() -> bool {
    let flags: usize;
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags)) };
    status::interrupts_masked(flags)
}

fn profile_ready() -> bool {
    let features = __cpuid(1);
    let (cr0, cr4, flags): (usize, usize, usize);
    unsafe {
        asm!("mov {}, cr0", "mov {}, cr4", "pushfq", "pop {}",
            out(reg) cr0, out(reg) cr4, out(reg) flags);
    }
    if !status::paging(cr0)
        || !status::interrupts_masked(flags)
        || flags & (NESTED_TASK | VIRTUAL_8086) != 0
        || cr4 & CET != 0
    {
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
            if !base_xcr0(u64::from(high) << 32 | u64::from(low)) {
                return false;
            }
            // XSAVES can add supervisor state outside XCR0.
            __cpuid_count(0x0d, 1).eax & (1 << 3) == 0 || supervisor_state_empty()
        }
    }
}

fn supervisor_state_empty() -> bool {
    const IA32_XSS: u32 = 0xda0;
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") IA32_XSS, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    low == 0 && high == 0
}

/// The current native IRQ. The controller separately establishes actual
/// hardware reception when it observes the original in-service vector.
pub struct Irq<'frame> {
    processor: ProcessorId,
    runtime: &'static RuntimeTrap,
    frame: PhantomData<&'frame mut *mut ()>,
}

impl<'frame> Irq<'frame> {
    fn new(
        _frame: &'frame mut Frame,
        processor: ProcessorId,
        runtime: &'static RuntimeTrap,
    ) -> Self {
        Self {
            processor,
            runtime,
            frame: PhantomData,
        }
    }

    pub fn processor(&self) -> ProcessorId {
        self.processor
    }

    pub fn vector(&self) -> u8 {
        self.runtime.vector
    }

    pub(in crate::metal) fn is_current(&self) -> bool {
        local::interrupt_local().is_some_and(|(processor, runtime)| {
            processor == self.processor && core::ptr::eq(runtime, self.runtime)
        })
    }
}

// The Rust view starts above the opaque FXSAVE area. Each integer slot and
// the five saved machine words were pushed on the independent IRQ stack.
#[repr(C)]
struct Frame {
    registers: [usize; 15],
    rip: usize,
    cs: usize,
    flags: usize,
    rsp: usize,
    ss: usize,
}

const _: () = assert!(size_of::<Frame>().is_multiple_of(16));

extern "sysv64" fn dispatch(frame: &mut Frame) {
    let Some((processor, runtime)) = local::interrupt_local() else {
        Terminal::early().reject(format_args!("FAIL Limen IRQ on an unadmitted entry\n"));
    };
    if !status::kernel_selector(frame.cs as u16) || frame.flags & (NESTED_TASK | VIRTUAL_8086) != 0
    {
        Terminal::early().reject(format_args!("FAIL Limen IRQ cannot return to this frame\n"));
    }
    (runtime.irq)(Irq::new(frame, processor, runtime));
}

#[unsafe(naked)]
unsafe extern "sysv64" fn vector() -> ! {
    naked_asm!(
        // The interrupt gate clears IF. IST provides the IRQ stack and
        // hardware supplies the five-word frame including original SS:RSP.
        "push rax",
        "push rcx",
        "push rdx",
        "push rbx",
        "push rbp",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, {fxsave}",
        "fxsave64 [rsp]",
        "fninit",
        "ldmxcsr [rip + {initial_mxcsr}]",
        "cld",
        "lea rdi, [rsp + {fxsave}]",
        "call {dispatch}",
        "fxrstor64 [rsp]",
        "add rsp, {fxsave}",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rbp",
        "pop rbx",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "iretq",
        fxsave = const FXSAVE_BYTES,
        initial_mxcsr = sym INITIAL_MXCSR,
        dispatch = sym dispatch,
    );
}
