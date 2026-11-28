//! Resident interrupt stacks and descriptor tables for one x86_64 Executor.

use core::{
    arch::asm,
    cell::UnsafeCell,
    mem::{MaybeUninit, offset_of, size_of},
    ops::Range,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::metal::processor::{
    description::x86_64::status,
    execution::terminal::Terminal,
    executor::x86_64::native::{self, Descriptor},
    treaty::Stack,
};

use super::super::InstallError;

const GDT_ENTRIES: usize = 512;
const IDT_ENTRIES: usize = 256;
const IRQ_BYTES: usize = 16 * 1024;
const EMERGENCY_BYTES: usize = 4096;

const IRQ_IST: u8 = 1;
const NMI_IST: u8 = 2;
const DOUBLE_FAULT_IST: u8 = 3;
const TERMINAL_IST: u8 = 4;
const NMI: u8 = 2;
const DOUBLE_FAULT: u8 = 8;

#[repr(C, packed)]
struct TaskState {
    reserved_start: u32,
    privilege: [u64; 3],
    reserved_privilege: u64,
    interrupt: [u64; 7],
    reserved_interrupt: u64,
    reserved_end: u16,
    io_map: u16,
}

impl TaskState {
    fn kernel(interrupt: [u64; 7]) -> Self {
        Self {
            reserved_start: 0,
            privilege: [0; 3],
            reserved_privilege: 0,
            interrupt,
            reserved_interrupt: 0,
            reserved_end: 0,
            io_map: size_of::<Self>() as u16,
        }
    }
}

const _: () = {
    assert!(size_of::<TaskState>() == 104);
    assert!(offset_of!(TaskState, interrupt) == 36);
    assert!(offset_of!(TaskState, io_map) == 102);
};

#[repr(C, packed)]
struct Gate {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_middle: u16,
    offset_high: u32,
    reserved: u32,
}

impl Gate {
    fn interrupt(address: usize, selector: u16, ist: u8) -> Self {
        Self {
            offset_low: address as u16,
            selector,
            ist,
            attributes: 0x8e,
            offset_middle: (address >> 16) as u16,
            offset_high: (address >> 32) as u32,
            reserved: 0,
        }
    }
}

const _: () = assert!(size_of::<Gate>() == 16);

#[derive(Clone, Copy)]
struct Layout {
    launch_base: usize,
    launch_limit: u16,
    code: u16,
    task: u16,
}

impl Layout {
    fn prepare() -> Option<Self> {
        let flags: usize;
        unsafe { asm!("pushfq", "pop {}", out(reg) flags) };
        let code = current_code();
        if !status::interrupts_masked(flags) || code < 8 || code & 7 != 0 {
            return None;
        }

        let descriptor = current_gdt();
        let bytes = usize::from(descriptor.limit) + 1;
        if !bytes.is_multiple_of(size_of::<u64>())
            || usize::from(code) + size_of::<u64>() > bytes
            || bytes + 2 * size_of::<u64>() > GDT_ENTRIES * size_of::<u64>()
            || descriptor.base == 0
            || descriptor.base.checked_add(bytes).is_none()
        {
            return None;
        }

        Some(Self {
            launch_base: descriptor.base,
            launch_limit: descriptor.limit,
            code,
            task: bytes as u16,
        })
    }

    fn active_gdt_limit(self) -> u16 {
        self.task + (2 * size_of::<u64>() - 1) as u16
    }

    fn original_launch(self) -> (Descriptor, u16) {
        (
            Descriptor {
                base: self.launch_base,
                limit: self.launch_limit,
            },
            self.code,
        )
    }
}

pub(super) struct Tables {
    irq: Stack<IRQ_BYTES>,
    nmi: Stack<EMERGENCY_BYTES>,
    double_fault: Stack<EMERGENCY_BYTES>,
    terminal: Stack<EMERGENCY_BYTES>,
    task: UnsafeCell<MaybeUninit<TaskState>>,
    gdt: UnsafeCell<MaybeUninit<[u64; GDT_ENTRIES]>>,
    idt: UnsafeCell<MaybeUninit<[Gate; IDT_ENTRIES]>>,
    layout: UnsafeCell<MaybeUninit<Layout>>,
    published: AtomicBool,
}

// The admitted PE initializes the tables once. Readback checks inspect
// the immutable layout only after publication. The CPU owns hardware
// writes to the GDT's access and busy bits.
unsafe impl Sync for Tables {}

impl Tables {
    pub(super) const fn new() -> Self {
        Self {
            irq: Stack::new(),
            nmi: Stack::new(),
            double_fault: Stack::new(),
            terminal: Stack::new(),
            task: UnsafeCell::new(MaybeUninit::uninit()),
            gdt: UnsafeCell::new(MaybeUninit::uninit()),
            idt: UnsafeCell::new(MaybeUninit::uninit()),
            layout: UnsafeCell::new(MaybeUninit::uninit()),
            published: AtomicBool::new(false),
        }
    }

    pub(super) fn stack(&self) -> Range<usize> {
        self.irq.range()
    }

    pub(super) fn entry(&self) -> native::Entry {
        native::Entry {
            base: self.idt.get().addr(),
            limit: (size_of::<[Gate; IDT_ENTRIES]>() - 1) as u16,
        }
    }

    pub(super) fn on_exception_stack(&self, sp: usize) -> bool {
        [
            self.stack(),
            self.nmi.range(),
            self.double_fault.range(),
            self.terminal.range(),
        ]
        .iter()
        .any(|stack| stack.contains(&sp))
    }

    pub(super) fn original_launch(&self) -> Option<(Descriptor, u16)> {
        self.layout().map(Layout::original_launch)
    }

    pub(super) fn matches(&self) -> bool {
        self.layout().is_some_and(|layout| self.matches_gdt(layout))
    }

    fn layout(&self) -> Option<Layout> {
        self.published
            .load(Ordering::Acquire)
            .then(|| unsafe { (*self.layout.get()).assume_init_read() })
    }

    /// # Safety
    ///
    /// The caller exclusively installs these resident tables on their
    /// admitted PE while ordinary interrupts are masked. The old IDT and
    /// GDT remain valid. The old GDT's full reported extent is readable and
    /// stable and does not overlap these tables. The selectors in the old
    /// IDT remain valid in its copy. The old GDT stays reserved and mapped
    /// for any later minor startup. All gate targets and the new tables
    /// and stacks stay resident while the machine may use them.
    pub(super) unsafe fn install(
        &'static self,
        irq_vector: u8,
        irq_entry: usize,
        fallback: impl Fn(u8) -> usize,
    ) -> Result<native::Entry, InstallError> {
        if irq_vector < 32 || irq_entry == 0 {
            return Err(InstallError::Context);
        }
        if self.published.load(Ordering::Acquire) {
            return Err(InstallError::Occupied);
        }
        let layout = Layout::prepare().ok_or(InstallError::Context)?;
        let entry = self.entry();
        let gdt = self.gdt.get().cast::<u64>();
        let idt = self.idt.get().cast::<Gate>();
        let task = self.task.get().cast::<TaskState>();
        let task_slot = usize::from(layout.task) / size_of::<u64>();
        let original = ptr::with_exposed_provenance::<u8>(layout.launch_base);

        unsafe {
            ptr::copy_nonoverlapping(original, gdt.cast::<u8>(), usize::from(layout.task));
        }
        let code = unsafe { gdt.add(usize::from(layout.code) / size_of::<u64>()).read() };
        if !long_kernel_code(code) {
            return Err(InstallError::Context);
        }
        for index in 0..IDT_ENTRIES {
            let vector = index as u8;
            let (address, ist) = match vector {
                vector if vector == irq_vector => (irq_entry, IRQ_IST),
                NMI => (fallback(vector), NMI_IST),
                DOUBLE_FAULT => (fallback(vector), DOUBLE_FAULT_IST),
                _ => (fallback(vector), TERMINAL_IST),
            };
            if address == 0 {
                return Err(InstallError::Context);
            }
            unsafe {
                idt.add(index)
                    .write(Gate::interrupt(address, layout.code, ist))
            };
        }

        let mut stacks = [0; 7];
        stacks[usize::from(IRQ_IST - 1)] = self.irq.top() as u64;
        stacks[usize::from(NMI_IST - 1)] = self.nmi.top() as u64;
        stacks[usize::from(DOUBLE_FAULT_IST - 1)] = self.double_fault.top() as u64;
        stacks[usize::from(TERMINAL_IST - 1)] = self.terminal.top() as u64;
        let [task_low, task_high] = task_descriptor(task.expose_provenance());
        unsafe {
            task.write(TaskState::kernel(stacks));
            gdt.add(task_slot).write(task_low);
            gdt.add(task_slot + 1).write(task_high);
        }
        let gdtr = Descriptor {
            base: gdt.expose_provenance(),
            limit: layout.active_gdt_limit(),
        };
        let idtr = Descriptor {
            base: idt.expose_provenance(),
            limit: entry.limit,
        };

        // Existing gates still use their original code selector, preserved
        // in the complete GDT copy. It is valid while LTR takes effect.
        unsafe {
            asm!(
                "lgdt [{table}]",
                "ltr {task:x}",
                table = in(reg) &gdtr,
                task = in(reg) layout.task,
                options(nostack, preserves_flags),
            );
        }
        if !self.matches_gdt(layout) {
            installation_failed();
        }
        // Every new gate has a live IST backed by the loaded TSS.
        unsafe { asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags)) };
        if native::entry() != Some(entry) {
            installation_failed();
        }
        unsafe { self.layout.get().write(MaybeUninit::new(layout)) };
        self.published.store(true, Ordering::Release);
        Ok(entry)
    }

    fn matches_gdt(&self, layout: Layout) -> bool {
        let task: u16;
        unsafe { asm!("str {:x}", out(reg) task, options(nostack, nomem, preserves_flags)) };
        let descriptor = current_gdt();
        if descriptor.base != self.gdt.get().addr()
            || descriptor.limit != layout.active_gdt_limit()
            || task != layout.task
            || current_code() != layout.code
        {
            return false;
        }
        let slot = usize::from(task) / size_of::<u64>();
        let gdt = self.gdt.get().cast::<u64>();
        let [low, high] = task_descriptor(self.task.get().addr());
        // LTR marks the loaded 64-bit available TSS descriptor busy.
        let busy = low | (1 << 41);
        unsafe {
            gdt.add(slot).read_volatile() == busy && gdt.add(slot + 1).read_volatile() == high
        }
    }
}

fn current_code() -> u16 {
    let code: u16;
    unsafe { asm!("mov {:x}, cs", out(reg) code, options(nostack, nomem, preserves_flags)) };
    code
}

fn current_gdt() -> Descriptor {
    let mut descriptor = Descriptor { base: 0, limit: 0 };
    unsafe { asm!("sgdt [{}]", in(reg) &mut descriptor, options(nostack, preserves_flags)) };
    descriptor
}

fn task_descriptor(base: usize) -> [u64; 2] {
    let base = base as u64;
    let limit = size_of::<TaskState>() as u64 - 1;
    let low = limit | ((base & 0x00ff_ffff) << 16) | (0x89 << 40) | (((base >> 24) & 0xff) << 56);
    [low, base >> 32]
}

fn long_kernel_code(descriptor: u64) -> bool {
    const PRESENT: u64 = 1 << 47;
    const CODE: u64 = (1 << 44) | (1 << 43);
    const DPL: u64 = 3 << 45;
    const LONG: u64 = 1 << 53;
    const DEFAULT_SIZE: u64 = 1 << 54;
    descriptor & (PRESENT | CODE | DPL | LONG | DEFAULT_SIZE) == PRESENT | CODE | LONG
}

fn installation_failed() -> ! {
    Terminal::early().reject(format_args!("FAIL Limen runtime interrupt tables\n"))
}
