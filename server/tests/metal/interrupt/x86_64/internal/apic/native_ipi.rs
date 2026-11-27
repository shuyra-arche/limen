use kernel::metal::platform::boot::treaty as boot_diagnostic;
use kernel::metal::processor::treaty::{Minor, Startup};
use kernel::metal::{
    processor::treaty::{LocalStorage, Stack},
    sync::treaty::Supply,
};

use core::sync::atomic::{
    AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use kernel::{
    entry::boot,
    metal::{
        interrupt::{
            treaty::{Issuance, NotifyHart},
            x86_64::internal::apic::{Failure, IpiTarget, LocalApic, Probe, Reception},
        },
        memory::access::treaty::device::DeviceWindow,
        platform::description::treaty::acpi::Madt,
    },
};
use limen_treaty::model::{ByteLength, InternalAddress, PhysicalAddress};
use uefi_fixture::{self as fixture, q35};

#[path = "../../window.rs"]
mod window;
use window::Window;

const VECTOR: u8 = 0xf3;
static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static ICR_PROBE: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicUsize = AtomicUsize::new(0);
static CLOSED: AtomicUsize = AtomicUsize::new(0);
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

fn await_vector(gate: Window) {
    let start = q35::tsc();
    let timeout = q35::tsc_frequency();
    while gate.count() == 0 {
        assert!(q35::tsc().wrapping_sub(start) < timeout);
        gate.permit();
    }
    assert_eq!(gate.count(), 1);
    // Restore the original diagnostic IDT before accepting the native ISR.
}

fn finish_claim(local: &mut LocalApic<'_>) {
    let mut claim = unsafe { local.receive(VECTOR) }.unwrap();
    assert_eq!(claim.vector(), VECTOR);
    unsafe { claim.complete() }.unwrap();
    assert_eq!(unsafe { claim.complete() }, Err(Failure::Busy));
    assert_eq!(local.reception(), Reception::Idle);
}

fn minor(hart: usize, argument: usize) -> ! {
    fixture::check_minor(&MINOR, hart);
    // The nonreturning major retains the immutable, ACPI-backed launch
    // descriptor until this Hart has fully retired its receive lease.
    let madt = unsafe { core::ptr::with_exposed_provenance::<Madt<'_>>(argument).read() };
    let address = madt.local_controller_address().unwrap();
    let physical = address.range(ByteLength::new(4096)).unwrap();
    // Both QEMU CPUs inherit the UC identity range. Local APIC access to
    // that address addresses this CPU's independent register bank.
    let mut window =
        unsafe { DeviceWindow::admit(physical, InternalAddress::new(address.get())) }.unwrap();
    unsafe { window::prepare(&mut window) };
    let mut local = unsafe { LocalApic::admit(hart, madt, window, Probe::native()) }
        .ok()
        .unwrap();
    let vector = local.reserve_vector(VECTOR).unwrap();
    let gate = unsafe { Window::install(vector.vector()) };
    ICR_PROBE.store(1, Release);
    await_vector(gate);
    finish_claim(&mut local);
    ICR_PROBE.store(2, Release);

    for round in 1..=2 {
        let gate = unsafe { Window::install(vector.vector()) };
        READY.store(round, Release);
        if round == 1 {
            MAILBOX.wait_for_issues(round, await_phase);
        }
        await_vector(gate);
        MAILBOX.recheck(round, || finish_claim(&mut local));
    }

    let gate = unsafe { Window::install(vector.vector()) };
    for _ in 0..4096 {
        gate.permit();
    }
    assert_eq!(gate.count(), 0);
    drop(gate);
    local.release_vector(vector).unwrap();
    CLOSED.store(1, Release);
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
    let original = IpiTarget::describe(madt, boot.hart(), VECTOR).unwrap();
    let launch = core::ptr::from_ref(&madt).expose_provenance();
    unsafe { MINOR.start(destination, minor, launch, &mut startup) }.unwrap();
    await_phase(&ICR_PROBE, 1);
    assert!(MINOR.ready());

    let Startup::Xapic {
        controller,
        trampoline,
        ..
    } = &mut startup;
    let page = PhysicalAddress::new(trampoline.addr().get() as u64);
    let issue = {
        let mut route = unsafe { controller.bind_ipi(target) }.unwrap();
        let Issuance::Pending(issued) = route.notify().unwrap() else {
            panic!("fixed ICR issue must be retained");
        };
        assert_eq!(issued.target(), target);
        issued
    };
    // The Harts may already have observed the physical effect. Only the
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
    boot_diagnostic::write(
        "PASS local-apic-ipi issued=3 rechecked=2 eoi=2 stale=0 scope-rejected=1 icr-blocked=1\n",
    );
}
