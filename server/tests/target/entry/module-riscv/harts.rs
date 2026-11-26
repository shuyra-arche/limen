//! Two-Hart execution and departure evidence for the module guest.

use core::{
    arch::asm,
    hint::spin_loop,
    ptr,
    sync::atomic::{
        AtomicPtr, AtomicU8, AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
    },
};
use kernel::{
    entry::boot::{Boot, Minor, Startup},
    metal::{
        execution::riscv::description::{RiscvExtension, RiscvIsa},
        hart::description::RiscvCpus,
        mmu::internal::riscv::{Mode, context::Context, description::RiscvMmu, root::Root},
        module::ModuleUse,
    },
};

use super::mapping;

const HART_STATE_MANAGEMENT: usize = 0x0048_534d;
const HART_STOP: usize = 1;
const HART_STATUS: usize = 2;
const STARTED: usize = 0;
const STOPPED: usize = 1;
const STOP_PENDING: usize = 3;

#[repr(u8)]
#[derive(Clone, Copy)]
enum Phase {
    Idle,
    Initial,
    FirstReturned,
    Successor,
    Departed,
}

static MINORS: [Minor; 3] = [const { Minor::new() }; 3];
static PHASE: AtomicU8 = AtomicU8::new(Phase::Idle as u8);
static ROOTS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static ENTRY: AtomicU64 = AtomicU64::new(0);
static ENCODER: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
static RETURNS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

pub fn check(boot: &Boot<'_>, modes: &[Mode]) -> usize {
    let major = boot.hart();
    assert!(major < 2);
    let minor = 1 - major;
    let tree = boot.device_tree_description().unwrap();
    let cpus = RiscvCpus::new(tree).unwrap().unwrap();
    let count = tree
        .nodes()
        .filter(|node| {
            node.parent_id() == Some(cpus.node().id())
                && node.string(b"device_type").unwrap() == Some(b"cpu")
        })
        .count();
    assert_eq!(count, 2);
    for hart in [major, minor] {
        let cpu = cpus.cpu(hart as u64).unwrap().unwrap();
        assert!(cpu.enabled());
        let mmu = RiscvMmu::describe(&cpu).unwrap().unwrap();
        assert!(modes.iter().all(|&mode| mmu.maximum().supports(mode)));
        let isa = RiscvIsa::describe(&cpu).unwrap().unwrap();
        assert!(isa.has(RiscvExtension::InstructionFence));
    }
    assert!(kernel::metal::firmware::sbi::platform_services().hart_state_management());
    assert_eq!(status(major), STARTED);
    assert_eq!(status(minor), STOPPED);
    minor
}

fn advance(phase: Phase) {
    PHASE.store(phase as u8, Release);
}

fn wait(phase: Phase) {
    while PHASE.load(Acquire) < phase as u8 {
        spin_loop();
    }
}

/// Calls the retained geometry provider and its typed dependent from both
/// live roots on the minor. The major callback runs after the initial calls
/// have returned. Both borrows remain live until firmware confirms that exact
/// Hart has left supervisor mode and stopped.
///
/// # Safety
///
/// The caller keeps both roots, the launch record, stacks and module maps
/// intact. Both contexts preserve the current image and the complete binding.
/// Only this fixture starts the minor, and it does not start it again after
/// the stop receipt. The held interface has the scalar geometry entry ABI.
pub unsafe fn exercise(
    peer: usize,
    roots: [Context; 2],
    held: &ModuleUse<'_>,
    encoder: &Root<'_>,
    first_returned: impl FnOnce(),
) -> [u64; 2] {
    assert_eq!(PHASE.load(Relaxed), Phase::Idle as u8);
    assert_ne!(roots[0], roots[1]);
    let mode = roots[0].mode().unwrap();
    assert_eq!(roots[1].mode(), Some(mode));
    let minor = &MINORS[mode.levels() - 3];
    for (place, root) in ROOTS.iter().zip(roots) {
        place.store(root.bits(), Relaxed);
    }
    ENTRY.store(held.entry(), Relaxed);
    ENCODER.store(shared_pointer(encoder), Relaxed);
    advance(Phase::Initial);
    unsafe { minor.start(peer, run, peer, Startup::Sbi) }.unwrap();
    wait(Phase::FirstReturned);
    assert!(minor.ready());
    assert_eq!(status(peer), STARTED);
    first_returned();
    advance(Phase::Successor);
    wait(Phase::Departed);
    loop {
        match status(peer) {
            STOPPED => break,
            STARTED | STOP_PENDING => spin_loop(),
            other => panic!("unexpected minor Hart state {other}"),
        }
    }
    ENCODER.store(ptr::null_mut(), Relaxed);
    advance(Phase::Idle);
    RETURNS.each_ref().map(|value| value.load(Relaxed))
}

fn shared_pointer<T: Sync>(value: &T) -> *mut () {
    ptr::from_ref(value).cast_mut().cast()
}

fn run(hart: usize, expected: usize) -> ! {
    assert_eq!(hart, expected);
    wait(Phase::Initial);
    let [initial, successor] = ROOTS
        .each_ref()
        .map(|root| Context::from_bits(root.load(Relaxed)).unwrap());
    let entry = ENTRY.load(Relaxed);
    // The two checked contexts preserve this stack, this code and the
    // published modules. Each switch synchronizes this Hart's own fetch.
    unsafe { mapping::switch_context(Context::BARE, initial) };
    let query = unsafe { core::mem::transmute::<u64, unsafe extern "C" fn() -> u64>(entry) };
    RETURNS[0].store(unsafe { query() }, Relaxed);
    let pointer = ENCODER.load(Relaxed).cast::<Root<'_>>();
    assert!(!pointer.is_null());
    // The launcher owns the shared borrow until HSM confirms our departure.
    let encoder = unsafe { &*pointer };
    let physical = successor.root().unwrap();
    assert_eq!(unsafe { encoder.context(physical) }, Some(successor));
    advance(Phase::FirstReturned);

    wait(Phase::Successor);
    unsafe { mapping::switch_context(initial, successor) };
    RETURNS[1].store(unsafe { query() }, Relaxed);
    assert_eq!(unsafe { encoder.context(physical) }, Some(successor));
    // Leave the shared address spaces and close prior translation and fetch
    // effects locally, before the major may edit either root.
    unsafe { mapping::switch_context(successor, Context::BARE) };
    advance(Phase::Departed);
    let (error, _) = hsm(HART_STOP, 0);
    panic!("minor HSM stop returned {error}");
}

fn status(hart: usize) -> usize {
    let (error, state) = hsm(HART_STATUS, hart);
    assert_eq!(error, 0);
    state
}

fn hsm(function: usize, argument: usize) -> (isize, usize) {
    let (error, value);
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") argument => error,
            lateout("a1") value,
            in("a6") function,
            in("a7") HART_STATE_MANAGEMENT,
            options(nostack),
        )
    };
    (error, value)
}
