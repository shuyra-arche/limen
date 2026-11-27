#![no_std]
#![no_main]

use core::sync::atomic::{
    AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use kernel::{
    entry::boot,
    metal::{
        interrupt::x86_64::internal::apic::{Failure, IpiTarget, LocalApic, Probe, Reception},
        memory::mmu::access::device::DeviceWindow,
        platform::description::acpi::Madt,
    },
};
use limen_treaty::model::{ByteLength, InternalAddress};
use uefi::prelude::*;
use uefi_fixture::{
    self as fixture,
    interrupt::{self, Window},
    q35,
};

const VECTOR: u8 = 0xf3;
static MINOR: boot::Minor = boot::Minor::new();
static READY: AtomicUsize = AtomicUsize::new(0);
static CLOSED: AtomicUsize = AtomicUsize::new(0);
static MAILBOX: ipi::Mailbox = ipi::Mailbox::new();

#[path = "../../../../support/ipi.rs"]
mod ipi;

fn await_phase(phase: &AtomicUsize, expected: usize) {
    let start = q35::tsc();
    let timeout = q35::tsc_frequency();
    while phase.load(Acquire) < expected {
        assert!(q35::tsc().wrapping_sub(start) < timeout);
        core::hint::spin_loop();
    }
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
    unsafe { interrupt::prepare(&mut window) };
    let mut local = unsafe { LocalApic::admit(hart, madt, window, Probe::native()) }
        .ok()
        .unwrap();
    let vector = local.reserve_vector(VECTOR).unwrap();
    for round in 1..=2 {
        let gate = unsafe { Window::install(vector.vector()) };
        READY.store(round, Release);
        MAILBOX.wait_for_issues(round, await_phase);
        let start = q35::tsc();
        let timeout = q35::tsc_frequency();
        while gate.count() == 0 {
            assert!(q35::tsc().wrapping_sub(start) < timeout);
            gate.permit();
        }
        assert_eq!(gate.count(), 1);
        // Production qualifies the receiving Hart against its actual
        // installed diagnostic IDT, so restore it before claiming the ISR.
        drop(gate);
        MAILBOX.recheck(round, || {
            let mut claim = unsafe { local.receive(VECTOR) }.unwrap();
            assert_eq!(claim.vector(), VECTOR);
            unsafe { claim.complete() }.unwrap();
            assert_eq!(unsafe { claim.complete() }, Err(Failure::Busy));
            assert_eq!(local.reception(), Reception::Idle);
        });
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

#[entry]
fn main() -> Status {
    fixture::load(q35::prepare_minor)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: boot::Boot<'_>) -> ! {
    let view = fixture::check_handoff(&boot, Some(&MINOR));
    let mut startup = fixture::inspect(&boot, &view);
    let madt = boot.acpi().unwrap().madt().unwrap();
    let destination = fixture::target();
    let target = IpiTarget::describe(madt, destination, VECTOR).unwrap();
    let original = IpiTarget::describe(madt, boot.hart(), VECTOR).unwrap();
    let launch = core::ptr::from_ref(&madt).expose_provenance();
    unsafe { MINOR.start(destination, minor, launch, &mut startup) }.unwrap();
    await_phase(&READY, 1);
    assert!(MINOR.ready());

    let boot::Startup::Xapic { controller, .. } = &mut startup;
    assert!(matches!(
        unsafe { controller.notify(original) },
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
    boot::write("PASS local-apic-ipi issued=3 rechecked=2 eoi=2 stale=0 scope-rejected=1\n");
    boot::shutdown(true)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fixture::report_panic(info)
}
