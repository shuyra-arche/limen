use kernel::entry::hook::Binding;
use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{InstallError, Irq, Minor, RuntimeTrap, Startup};
use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::{Supply, TurnAcquireError, TurnLock},
};

use core::{
    arch::{asm, naked_asm},
    cell::UnsafeCell,
    fmt::Write,
    sync::atomic::{
        AtomicUsize,
        Ordering::{Acquire, Relaxed, Release},
        fence,
    },
};
use kernel::{
    entry::boot,
    metal::{
        interrupt::{
            treaty::{HardwareClaim, Issuance, NotifyProcessor},
            x86_64::internal::apic::{
                Failure, IpiTarget, LocalApic, Probe, Reception, VectorLease,
            },
        },
        memory::access::treaty::device::DeviceWindow,
        platform::description::treaty::acpi::Madt,
    },
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};
use uefi_fixture::{self as fixture, q35};

#[path = "../../prepare.rs"]
mod hardware;

const VECTOR: u8 = 0xf3;
static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static ICR_PROBE: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicUsize = AtomicUsize::new(0);
static CLOSED: AtomicUsize = AtomicUsize::new(0);
static CAPTURED: AtomicUsize = AtomicUsize::new(0);
static TURN: TurnLock<()> = TurnLock::new(());
static TRAP: RuntimeTrap = RuntimeTrap::new(VECTOR, RECEPTION.entry());
static MAJOR_TRAP: RuntimeTrap = RuntimeTrap::new(VECTOR, TERMINAL_ON_IRQ.entry());
static TERMINAL_ON_IRQ: Binding<()> = Binding::new((), unexpected_irq);

struct Receiver {
    processor: usize,
    controller: LocalApic<'static>,
    original: VectorLease,
    stack_top: usize,
}

struct LocalReceiver(UnsafeCell<Option<Receiver>>);

// Only the minor owns this slot. Its ordinary accesses end before IF opens,
// and the non-nesting IRQ borrow ends before the interrupted code resumes.
unsafe impl Sync for LocalReceiver {}

static RECEPTION: Binding<LocalReceiver> =
    Binding::new(LocalReceiver(UnsafeCell::new(None)), interrupt);

static NMI_READY: AtomicUsize = AtomicUsize::new(0);
static NMI_RESUME: AtomicUsize = AtomicUsize::new(0);
static NMI_LIMIT: AtomicUsize = AtomicUsize::new(0);

fn terminal_case() -> Option<&'static str> {
    option_env!("LIMEN_X86_TRAP_CASE")
}

#[unsafe(naked)]
unsafe extern "C" fn wait_on_hostile_stack() -> ! {
    naked_asm!(
        "lea rax, [rip + 2f]",
        "mov [rip + {resume}], rax",
        "lea rax, [rip + 3f]",
        "mov [rip + {limit}], rax",
        // Interrupts remain masked. Only the remotely issued NMI should
        // arrive here. The interrupted stack cannot accept a hardware frame.
        "xor esp, esp",
        "mov qword ptr [rip + {ready}], 1",
        "2: pause",
        "jmp 2b",
        "3: ud2",
        ready = sym NMI_READY,
        resume = sym NMI_RESUME,
        limit = sym NMI_LIMIT,
    );
}

// The disposable test image owns this per-processor copy. Its #DF gate
// still uses the actual installed runtime descriptor and permanent IST.
static mut FAULT_IDT: [[u8; 16]; 256] = [[0; 16]; 256];

fn deliver_double_fault() -> ! {
    const GENERAL_PROTECTION: usize = 13;
    let original = q35::Idtr::read();
    assert_eq!({ original.limit }, 4095);
    let replacement = (&raw mut FAULT_IDT).cast::<[u8; 16]>();
    unsafe { replacement.copy_from_nonoverlapping(original.base as *const [u8; 16], 256) };
    let original_gp = unsafe { &mut *replacement.add(GENERAL_PROTECTION) };
    assert_eq!(original_gp[5], 0x8e);
    // Force #GP again during delivery of the first #GP. The original #DF
    // slot and its independent stack retain the installed runtime entry.
    original_gp[2..4].copy_from_slice(&0u16.to_le_bytes());
    let changed = q35::Idtr {
        limit: original.limit,
        base: replacement.addr(),
    };
    boot_diagnostic::console()
        .unwrap()
        .write_fmt(format_args!(
            "EXPECT Limen double fault: gp={:#x} stack=0x0\n",
            invalid_stack_segment as *const () as usize
        ))
        .unwrap();
    unsafe { trigger_general_protection(&changed) }
}

#[unsafe(naked)]
unsafe extern "sysv64" fn trigger_general_protection(_idt: *const q35::Idtr) -> ! {
    naked_asm!(
        "lidt [rdi]",
        "xor esp, esp",
        "mov eax, 0xfff8",
        "jmp {site}",
        site = sym invalid_stack_segment,
    );
}

#[unsafe(naked)]
unsafe extern "sysv64" fn invalid_stack_segment() -> ! {
    naked_asm!("mov ss, ax", "ud2");
}

static MAILBOX: ipi::Mailbox = ipi::Mailbox::new();

#[path = "../../../ipi.rs"]
mod ipi;

fn await_phase(phase: &AtomicUsize, expected: usize) {
    let start = q35::tsc();
    let timeout = q35::tsc_frequency();
    while phase.load(Acquire) < expected {
        assert!(q35::tsc().wrapping_sub(start) < timeout);
        core::hint::spin_loop();
    }
}

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: usize,
}

fn actual_ist(vector: u8) -> (u8, usize) {
    let idtr = q35::Idtr::read();
    assert_eq!({ idtr.limit }, 4095);
    let gate = (idtr.base + 16 * usize::from(vector)) as *const [u8; 16];
    let gate = unsafe { gate.read_volatile() };
    assert_eq!(gate[5], 0x8e);
    let selector = gate[4] & 7;
    assert_ne!(selector, 0, "the hardware gate must select IST");

    let mut gdtr = Gdtr { limit: 0, base: 0 };
    let task: u16;
    unsafe {
        asm!("sgdt [{}]", "str {:x}", in(reg) &mut gdtr, out(reg) task,
             options(nostack, preserves_flags));
    }
    let offset = usize::from(task & !7);
    assert_eq!(task & 7, 0);
    assert!(offset != 0 && offset + 15 <= usize::from(gdtr.limit));
    let tss_descriptor = (gdtr.base + offset) as *const [u8; 16];
    let descriptor = unsafe { tss_descriptor.read_volatile() };
    assert_eq!(descriptor[5], 0x8b, "LTR consumed the available TSS");
    let tss = usize::from(descriptor[2])
        | usize::from(descriptor[3]) << 8
        | usize::from(descriptor[4]) << 16
        | usize::from(descriptor[7]) << 24
        | (u32::from_le_bytes(descriptor[8..12].try_into().unwrap()) as usize) << 32;
    // Long-mode TSS IST1 begins at byte 36, before the remaining IST slots.
    let slot = (tss + 36 + (usize::from(selector) - 1) * 8) as *const usize;
    (selector, unsafe { slot.read_unaligned() })
}

fn unexpected_irq(_: &(), _: Irq<'_>) {
    panic!("the source processor never opens its runtime IRQ gate");
}

fn interrupt(owner: &LocalReceiver, mut irq: Irq<'_>) {
    assert_eq!(TURN.acquire(|_| ()), Err(TurnAcquireError::Context));
    let receiver = unsafe { (&mut *owner.0.get()).as_mut().expect("admitted vector") };
    assert_eq!(irq.processor().get(), receiver.processor);
    assert_eq!(irq.vector(), receiver.original.vector());
    let stack: usize;
    unsafe { asm!("mov {}, rsp", out(reg) stack, options(nostack, nomem, preserves_flags)) };
    assert!(!MINOR.stack_range().contains(&stack));
    assert!((1..=16 * 1024).contains(&receiver.stack_top.checked_sub(stack).unwrap()));

    let mut claim = unsafe { receiver.controller.receive_irq(&mut irq) }.unwrap();
    assert_eq!(claim.vector(), receiver.original.vector());
    assert_eq!(claim.identity().round(), CAPTURED.load(Relaxed) as u64 + 1);
    unsafe { claim.complete() }.unwrap();
    assert_eq!(unsafe { claim.complete() }, Err(Failure::Busy));

    // Registers caller-saved by Rust still belong to the interrupted code.
    unsafe {
        asm!("xor r8d, r8d", "pxor xmm0, xmm0", out("r8") _, out("xmm0") _, options(nostack, nomem));
    }
    CAPTURED.fetch_add(1, Release);
}

fn permit_irq() {
    let flags: usize;
    let integer: usize;
    let stack: usize;
    let vectors: usize;
    unsafe {
        asm!(
            "mov r8, 0x6123456789abcdef",
            "mov r9, rsp",
            "pcmpeqd xmm0, xmm0",
            "psllq xmm0, 3",
            "cmp r8, r8",
            "stc",
            "std",
            // Interrupts may enter after the NOP or PUSHFQ. Both are
            // ordinary continuation instructions in this exact window.
            "sti",
            "nop",
            "pushfq",
            "cli",
            "pop rdx",
            "cld",
            "mov r10, rsp",
            "xor ecx, ecx",
            "cmp r9, r10",
            "sete cl",
            "pcmpeqd xmm1, xmm1",
            "psllq xmm1, 3",
            "pcmpeqb xmm1, xmm0",
            "pmovmskb eax, xmm1",
            lateout("rdx") flags,
            lateout("rax") vectors,
            lateout("rcx") stack,
            lateout("r8") integer,
            out("r9") _,
            out("r10") _,
            out("xmm0") _,
            out("xmm1") _,
        );
    }
    const EXPECTED_FLAGS: usize = (1 << 10) | (1 << 9) | (1 << 6) | 1;
    assert_eq!(flags & EXPECTED_FLAGS, EXPECTED_FLAGS);
    assert_eq!(integer, 0x6123_4567_89ab_cdef);
    assert_eq!(stack, 1);
    assert_eq!(vectors, 0xffff);
}

fn await_vector(expected: usize) {
    let start = q35::tsc();
    let timeout = q35::tsc_frequency();
    while CAPTURED.load(Acquire) < expected {
        assert!(q35::tsc().wrapping_sub(start) < timeout);
        permit_irq();
    }
    assert_eq!(CAPTURED.load(Acquire), expected);
    assert_eq!(TURN.acquire(|_| ()), Ok(()));
}

fn minor(processor: usize, argument: usize) -> ! {
    fixture::check_minor(&MINOR, processor);
    // The nonreturning major retains the immutable, ACPI-backed launch
    // descriptor until this processor has fully retired its receive lease.
    let madt = unsafe { core::ptr::with_exposed_provenance::<Madt<'_>>(argument).read() };
    let address = madt.local_controller_address().unwrap();
    let physical = address.range(ByteLength::new(4096)).unwrap();
    // Both QEMU CPUs inherit the UC identity range. Local APIC access to
    // that address addresses this CPU's independent register bank.
    let mut window =
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(address.get())) }.unwrap();
    unsafe { hardware::prepare(&mut window) };
    let mut local = unsafe { LocalApic::admit(processor, madt, window, Probe::native()) }
        .ok()
        .unwrap();
    let original = local.reserve_vector(VECTOR).unwrap();
    unsafe { TRAP.install() }.unwrap();
    assert_eq!(unsafe { TRAP.install() }, Err(InstallError::Occupied));
    let (irq_slot, stack_top) = actual_ist(VECTOR);
    assert_ne!(stack_top, 0);
    assert_eq!(stack_top % 16, 0);
    assert!(!MINOR.stack_range().contains(&stack_top));
    let (nmi_slot, nmi_stack) = actual_ist(2);
    let (double_fault_slot, double_fault_stack) = actual_ist(8);
    for (slot, top) in [
        (nmi_slot, nmi_stack),
        (double_fault_slot, double_fault_stack),
    ] {
        assert_ne!(slot, irq_slot);
        assert_ne!(top, stack_top);
    }
    assert_ne!(nmi_slot, double_fault_slot);
    assert_ne!(nmi_stack, double_fault_stack);
    unsafe {
        RECEPTION.owner().0.get().write(Some(Receiver {
            processor,
            controller: local,
            original,
            stack_top,
        }))
    };
    assert_eq!(TURN.acquire(|_| ()), Ok(()));
    ICR_PROBE.store(1, Release);
    await_vector(1);
    ICR_PROBE.store(2, Release);

    for round in 1..=2 {
        READY.store(round, Release);
        if round == 1 {
            MAILBOX.wait_for_issues(round, await_phase);
        }
        await_vector(round + 1);
        MAILBOX.recheck(round, || {
            let receiver = unsafe { (&*RECEPTION.owner().0.get()).as_ref().unwrap() };
            assert_eq!(receiver.controller.reception(), Reception::Idle);
        });
    }

    for _ in 0..4096 {
        permit_irq();
    }
    assert_eq!(CAPTURED.load(Acquire), 3);
    let Receiver {
        mut controller,
        original,
        ..
    } = unsafe {
        (&mut *RECEPTION.owner().0.get())
            .take()
            .expect("retired IRQ context")
    };
    controller.release_vector(original).unwrap();
    assert_eq!(controller.reception(), Reception::Idle);
    match terminal_case() {
        Some("nmi") => {
            CLOSED.store(1, Release);
            unsafe { wait_on_hostile_stack() };
        }
        Some("double-fault") => deliver_double_fault(),
        Some(case) => panic!("unknown runtime terminal probe {case}"),
        None => CLOSED.store(1, Release),
    }
    loop {
        core::hint::spin_loop();
    }
}

fn deliver_nmi(boot: &boot::Boot<'_>, destination: usize) {
    await_phase(&NMI_READY, 1);
    boot_diagnostic::console()
        .unwrap()
        .write_fmt(format_args!(
            "EXPECT Limen NMI: first={:#x} limit={:#x} stack=0x0\n",
            NMI_RESUME.load(Relaxed),
            NMI_LIMIT.load(Relaxed)
        ))
        .unwrap();
    // All earlier startup, ICR and reception operations settled and the
    // source controller borrow has ended. Reserve this fixture's last ICR
    // operation until the actual diagnostic exit.
    let mut window = q35::local_window(boot);
    const ICR_LOW: usize = 0x300;
    const ICR_HIGH: usize = 0x310;
    const DELIVERING: u32 = 1 << 12;
    const NMI: u32 = 4 << 8 | 1 << 14;
    assert_eq!(unsafe { window.read_u32(ICR_LOW) }.unwrap() & DELIVERING, 0);
    fence(Release);
    unsafe {
        window
            .write_u32(ICR_HIGH, (destination as u32) << 24)
            .unwrap();
        window.write_u32(ICR_LOW, NMI).unwrap();
    }
    loop {
        core::hint::spin_loop();
    }
}

pub fn check(boot: boot::Boot<'_>) {
    assert!(!MINOR.ready());
    assert_eq!(ICR_PROBE.load(Acquire), 0);
    assert_eq!(READY.load(Acquire), 0);
    assert_eq!(CLOSED.load(Acquire), 0);

    let view = fixture::check_handoff(&boot, Some(&MINOR));
    let mut startup = fixture::inspect(&boot, &view);
    let madt = boot.acpi().unwrap().madt().unwrap();
    assert_eq!(madt.processors().filter(|cpu| cpu.enabled()).count(), 2);
    let destination = fixture::target();
    let target = IpiTarget::describe(madt, destination, VECTOR).unwrap();
    let original = IpiTarget::describe(madt, boot.processor(), VECTOR).unwrap();
    let launch = core::ptr::from_ref(&madt).expose_provenance();
    unsafe { MAJOR_TRAP.install() }.unwrap();
    let (_, major_stack) = actual_ist(VECTOR);
    assert!(!boot.stack_range().contains(&major_stack));
    // Startup must retain the live selector and GDT after BSP installation.
    unsafe { MINOR.start(destination, minor, launch, &mut startup) }.unwrap();
    await_phase(&ICR_PROBE, 1);
    assert!(MINOR.ready());

    {
        let Startup::Xapic {
            mut controller,
            trampoline,
            ..
        } = startup;
        let page = PhysicalAddress::new(trampoline.addr().get() as u64);
        let issue = {
            let mut route = unsafe { controller.bind_ipi(target) }.unwrap();
            let Issuance::Pending(issued) = route.notify().unwrap() else {
                panic!("fixed ICR issue must be retained");
            };
            assert_eq!(issued.target(), target);
            issued
        };
        // The processors may already have observed the physical effect. Only the
        // original send observation can settle the sender's ICR obligation.
        assert!(matches!(
            controller.prepare(destination, page),
            Err(Failure::Busy)
        ));
        {
            let mut route = unsafe { controller.bind_ipi(target) }.unwrap();
            assert_eq!(route.pending(), Ok(Some(issue)));
            assert_eq!(route.confirm(issue), Ok(Issuance::Confirmed(issue)));
        }
        await_phase(&ICR_PROBE, 2);

        assert!(matches!(
            unsafe { controller.bind_ipi(original) },
            Err(Failure::Scope)
        ));
        for round in 1..=2 {
            await_phase(&READY, round);
            let mut route = unsafe { controller.bind_ipi(target) }.unwrap();
            MAILBOX.publish(&mut route, round, round == 1);
            MAILBOX.wait_for_recheck(round, await_phase);
        }
        await_phase(&CLOSED, 1);
        assert_eq!(controller.reception(), Reception::Idle);
        assert_eq!(READY.load(Relaxed), 2);
    }
    if terminal_case() == Some("nmi") {
        deliver_nmi(&boot, destination);
    }
    boot_diagnostic::write(
        "PASS local-apic-ipi capture=3 rechecked=2 eoi=3 restored=3 ist=1 turn-denied=3 stale=0 scope-rejected=1 icr-blocked=1\n",
    );
}
