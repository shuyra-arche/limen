#![no_std]
#![no_main]

#[path = "../../../../../support/paging/retirement.rs"]
mod retirement;

use core::{arch::asm, fmt::Write};
use kernel::metal::memory::mmu::{Retirement, Unbind};
use kernel::{
    entry::boot::{self, Boot},
    metal::memory::mmu::{
        access::device::DeviceWindow,
        riscv64::external::iommu::{
            self, Context, Controller, DeviceOrder, Directory, Effect, Refresh, Sequence, Storage,
        },
    },
};
use limen_treaty::model::{InternalAddress, PhysicalAddress};

#[repr(align(4096))]
struct Page([u64; 512]);
static mut COMMANDS: iommu::queue::Page = iommu::queue::Page::EMPTY;
static mut DIRECTORY: iommu::directory::Page = iommu::directory::Page::EMPTY;
static mut BINDINGS: [Option<iommu::Binding>; 3] = [None; 3];
static mut SEQUENCE: Sequence = Sequence::FRESH;
static mut TABLES: [[Page; 3]; 2] = [const { [const { Page([0; 512]) }; 3] }; 2];
static mut PAYLOADS: [Page; 2] = [const { Page([0; 512]) }; 2];
const SECOND_RID: u32 = 7 << 3;
#[path = "../../../../../support/pci/ecam.rs"]
mod pci;

fn ticks() -> u64 {
    let value: u64;
    unsafe { asm!("rdtime {}", out(reg) value, options(nostack)) };
    value
}
fn physical<T>(value: *const T) -> PhysicalAddress {
    PhysicalAddress::new(value.addr() as u64)
}

// Hardware debug translation for DID 0, no process ID. Keeping this in the
// guest avoids turning a debug interface into ordinary mapping authority.
fn translate(window: &mut DeviceWindow<'_>, iova: u64, read_only: bool) -> Option<u64> {
    const IOVA: usize = 0;
    const CONTROL: usize = 8;
    const RESULT: usize = 16;
    unsafe {
        assert_eq!(window.read_u64(CONTROL).unwrap() & 1, 0);
        window.write_u64(IOVA, iova).unwrap();
        order();
        window
            .write_u64(CONTROL, 1 | (u64::from(read_only) << 3))
            .unwrap();
        order();
        for _ in 0..100_000 {
            if window.read_u64(CONTROL).unwrap() & 1 == 0 {
                order();
                let value = window.read_u64(RESULT).unwrap();
                if value & 1 != 0 {
                    return None;
                }
                assert_eq!(value & (1 << 9), 0);
                return Some((value >> 10 & ((1 << 44) - 1)) << 12);
            }
        }
    }
    panic!("debug translation did not complete");
}

fn order() {
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}

fn retry<T>(mut f: impl FnMut() -> Result<T, iommu::Error>) -> T {
    for _ in 0..100_000 {
        match f() {
            Ok(v) => return v,
            Err(iommu::Error::Busy) => core::hint::spin_loop(),
            Err(error) => panic!("IOMMU control failed: {error:?}"),
        }
    }
    panic!("IOMMU acknowledgement timeout")
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut console) = boot::console() {
        let _ = writeln!(console, "FAIL IOMMU {info}");
    }
    boot::shutdown(false)
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot<'_>) -> ! {
    let tree = boot.device_tree_description().unwrap();
    let mut range = None;
    for node in tree.nodes() {
        if let Some(registers) = iommu::registers(node).unwrap() {
            assert!(range.replace(registers).is_none());
        }
    }
    let range = range.expect("a platform IOMMU");
    // The VM has two quiesced EDU functions, no PCI ATS, and Bare SATP.
    let hertz = kernel::metal::chrono::riscv64::RiscvTimebase::describe(boot.riscv_cpus())
        .unwrap()
        .unwrap()
        .hertz();
    let mut endpoints = [
        pci::Edu::open(
            &tree,
            range,
            pci::RID,
            Some(0),
            order,
            ticks,
            u64::from(hertz),
        ),
        pci::Edu::open(
            &tree,
            range,
            SECOND_RID,
            Some(1),
            order,
            ticks,
            u64::from(hertz),
        ),
    ];
    let mut window =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() };
    let root = physical(&raw mut DIRECTORY);
    let storage = Storage {
        page: unsafe { &mut *(&raw mut DIRECTORY) },
        physical: root,
        bindings: unsafe { &mut *(&raw mut BINDINGS) },
        sequence: unsafe { &mut *(&raw mut SEQUENCE) },
    };
    let mut controller =
        unsafe { Controller::admit(&mut window, DeviceOrder::native(), storage) }.unwrap();
    let profile = controller.profile();
    assert!(profile.sv39 && profile.debug_translation);
    unsafe { controller.begin_shut() }.unwrap();
    assert_eq!(retry(|| controller.observe_directory()), Directory::Off);
    // Use the same IOVA in two requester contexts with independent physical pages.
    const IOVA: u32 = 0x1000;
    const PATTERNS: [u32; 2] = [0xbe71_6a29, 0xac11_378d];
    let payloads = (&raw mut PAYLOADS).cast::<Page>();

    let tables = unsafe { &mut *(&raw mut TABLES) };
    let mut words = [core::ptr::null_mut::<u32>(); 2];
    for (index, did) in [pci::RID, SECOND_RID].into_iter().enumerate() {
        let payload = unsafe { payloads.add(index) };
        assert_ne!(u64::from(IOVA), physical(payload).get());
        words[index] = payload.cast();
        let pages = &mut tables[index];
        pages[0].0[0] = profile.branch(physical(&pages[1])).unwrap();
        pages[1].0[0] = profile.branch(physical(&pages[2])).unwrap();
        pages[2].0[1] = profile
            .leaf(physical(payload), iommu::translation::Access::ReadWrite)
            .unwrap();
        let context = Context {
            device: did,
            root: physical(&pages[0]),
            pscid: (index + 1) as u32,
        };
        unsafe { controller.prepare(context) }.unwrap();
        if index == 0 {
            unsafe {
                controller.prepare(Context {
                    device: 0,
                    ..context
                })
            }
            .unwrap();
        }
    }
    assert_eq!(
        unsafe {
            controller.prepare(Context {
                device: 128,
                root,
                pscid: 1,
            })
        },
        Err(iommu::Error::Address)
    );
    unsafe { controller.begin_install() }.unwrap();
    assert_eq!(retry(|| controller.observe_directory()), Directory::One);
    let mut registers = controller.debug_registers().unwrap();
    assert_eq!(
        translate(&mut registers, 0x1000, true),
        Some(physical(payloads).get())
    );
    assert_eq!(translate(&mut registers, 0x2000, true), None);
    assert_eq!(
        unsafe { controller.begin_install() },
        Err(iommu::Error::Mode)
    );
    for (index, endpoint) in endpoints.iter_mut().enumerate() {
        unsafe { words[index].write_volatile(PATTERNS[index]) };
        order();
        endpoint.transfer(IOVA, false);
    }
    for (index, endpoint) in endpoints.iter_mut().enumerate() {
        for (word, pattern) in words.into_iter().zip(PATTERNS) {
            unsafe { word.write_volatile(!pattern) };
        }
        order();
        endpoint.transfer(IOVA, true);
        assert_eq!(unsafe { words[index].read_volatile() }, PATTERNS[index]);
        assert_eq!(
            unsafe { words[1 - index].read_volatile() },
            !PATTERNS[1 - index]
        );
    }
    let leaf = profile
        .leaf(physical(payloads), iommu::translation::Access::Read)
        .unwrap();
    unsafe { (&raw mut tables[0][2].0[1]).write_volatile(leaf) };
    order();
    let original = controller.binding(pci::RID).unwrap();
    let other = controller.binding(SECOND_RID).unwrap();
    let diagnostic = controller.binding(0).unwrap();
    let commands = unsafe { &mut *(&raw mut COMMANDS) };
    let address = PhysicalAddress::new(core::ptr::from_ref(commands).addr() as u64);
    unsafe { controller.open_queue(commands, address) }.unwrap();
    let mut previous = None;
    for i in 0..130 {
        let scope = if i % 2 == 0 {
            Refresh::Directory
        } else {
            Refresh::FirstStage
        };
        let command = unsafe { controller.begin_refresh(original, scope) }.unwrap();
        let effect = Effect::Refreshed(original, scope);
        assert_eq!(controller.unsettled_effect(), Some(effect));
        assert_eq!(
            unsafe { controller.begin_refresh(original, scope) },
            Err(iommu::Error::Busy)
        );
        if let Some(previous) = previous {
            assert_eq!(controller.observe(previous), Err(iommu::Error::Stale));
        }
        assert_eq!(retry(|| controller.observe(command)), effect);
        assert_eq!(controller.observe(command), Err(iommu::Error::Stale));
        previous = Some(command);
    }
    for (word, pattern) in words.into_iter().zip(PATTERNS) {
        unsafe { word.write_volatile(!pattern) };
    }
    order();
    endpoints[0].transfer(IOVA, true);
    assert_eq!(unsafe { words[0].read_volatile() }, !PATTERNS[0]);
    endpoints[1].transfer(IOVA, true);
    assert_eq!(unsafe { words[0].read_volatile() }, !PATTERNS[0]);
    assert_eq!(unsafe { words[1].read_volatile() }, PATTERNS[1]);
    let (page, queue) = retry(|| controller.shut_queue());
    assert_eq!(queue, address);
    unsafe { controller.open_queue(page, queue) }.unwrap();
    let unbind = unsafe { controller.begin_unbind(original) }.unwrap();
    assert_eq!(
        controller.observe(previous.unwrap()),
        Err(iommu::Error::Stale)
    );
    assert_eq!(controller.binding(pci::RID), None);
    assert_eq!(controller.pending_command(), Some(unbind));
    assert!(controller.pending_retirement(other).is_none());
    let recovered = controller.pending_retirement(original).unwrap();
    let settled = retry(|| match controller.observe_retirement(&recovered) {
        Ok(Retirement::Pending) => Err(iommu::Error::Busy),
        result => result,
    });
    assert_eq!(settled, Retirement::Settled);
    assert_eq!(
        controller.observe_retirement(&recovered),
        Err(iommu::Error::Stale)
    );
    assert!(controller.pending_command().is_none());
    unsafe { words[0].write_volatile(!PATTERNS[0]) };
    order();
    endpoints[0].transfer(IOVA, true);
    assert_eq!(unsafe { words[0].read_volatile() }, !PATTERNS[0]);
    let bind = unsafe { controller.begin_bind(original.context()) }.unwrap();
    let Effect::Bound(replacement) = retry(|| controller.observe(bind)) else {
        panic!("wrong binding effect");
    };
    assert_ne!(replacement, original);
    assert_eq!(
        unsafe { controller.begin_unbind(original) },
        Err(iommu::Error::Stale)
    );
    for binding in [replacement, other, diagnostic] {
        unsafe { retirement::retire(&mut controller, binding, |_| {}) };
    }
    let (returned, original) = retry(|| controller.shut_queue());
    assert_eq!(original, address);
    assert_eq!(core::ptr::from_ref(returned).addr() as u64, original.get());
    assert_eq!(controller.retained_queue(), None);
    unsafe { controller.begin_shut() }.unwrap();
    assert_eq!(retry(|| controller.observe_directory()), Directory::Off);
    assert_eq!(controller.retained_root(), Some(root));
    assert_eq!(
        unsafe { controller.begin_install() },
        Err(iommu::Error::Mode)
    );
    boot::write(
        "\nPASS riscv-iommu sv39=1 directory=1 replacement-rejected=1 off=1 fences=130 walk=1 fault=1 dma=1 downgrade=1 bindings=2 unbound=1 stale=1 recovered=1 queue-return=2\n",
    );
    boot::shutdown(true)
}
