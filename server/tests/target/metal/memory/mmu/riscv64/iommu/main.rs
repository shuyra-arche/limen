#![no_std]
#![no_main]

use core::{arch::asm, fmt::Write};
use kernel::{
    entry::boot::{self, Boot},
    metal::memory::mmu::{
        access::device::DeviceWindow,
        riscv64::external::iommu::{self, Controller, DeviceOrder, Directory},
    },
};
use limen_treaty::model::{InternalAddress, PhysicalAddress};

#[repr(align(4096))]
struct Page([u64; 512]);
static mut COMMANDS: iommu::queue::Page = iommu::queue::Page::EMPTY;
static mut DIRECTORY: Page = Page([0; 512]);
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
fn translate(window: &mut DeviceWindow, iova: u64, read_only: bool) -> Option<u64> {
    const IOVA: usize = 0x258;
    const CONTROL: usize = 0x260;
    const RESULT: usize = 0x268;
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
    let mut controller = unsafe { Controller::admit(&mut window, DeviceOrder::native()) }.unwrap();
    let profile = controller.profile();
    assert!(profile.sv39 && profile.debug_translation);
    unsafe { controller.begin_shut() }.unwrap();
    assert_eq!(retry(|| controller.observe_directory()), Directory::Off);
    // Use the same IOVA in two requester contexts with independent physical pages.
    const IOVA: u32 = 0x1000;
    const PATTERNS: [u32; 2] = [0xbe71_6a29, 0xac11_378d];
    let payloads = (&raw mut PAYLOADS).cast::<Page>();
    let directory = unsafe { &mut *(&raw mut DIRECTORY) };
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
        let context = profile
            .context(physical(&pages[0]), (index + 1) as u32)
            .unwrap();
        directory.0[profile.context_words(did).unwrap()][..4].copy_from_slice(&context);
        if index == 0 {
            directory.0[profile.context_words(0).unwrap()][..4].copy_from_slice(&context);
        }
    }
    assert!(profile.context_words(128).is_err());
    let root = physical(directory);
    unsafe { controller.begin_install(root) }.unwrap();
    assert_eq!(retry(|| controller.observe_directory()), Directory::One);
    let registers = unsafe { controller.debug_registers() }.unwrap();
    assert_eq!(
        translate(registers, 0x1000, true),
        Some(physical(payloads).get())
    );
    assert_eq!(translate(registers, 0x2000, true), None);
    assert_eq!(
        unsafe { controller.begin_install(root) },
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
    use iommu::queue::Invalidate;
    let commands = unsafe { &mut *(&raw mut COMMANDS) };
    let address = PhysicalAddress::new(core::ptr::from_ref(commands).addr() as u64);
    unsafe { controller.open_queue(commands, address) }.unwrap();
    assert_eq!(
        controller.submit(Invalidate::Device(1 << 24)),
        Err(iommu::Error::Address)
    );
    for i in 0..130 {
        let operation = if i % 2 == 0 {
            Invalidate::Device(pci::RID)
        } else {
            Invalidate::FirstStage
        };
        retry(|| controller.submit(operation));
        assert_eq!(controller.submit(operation), Err(iommu::Error::Mode));
        assert_eq!(retry(|| controller.observe()), operation);
        assert_eq!(controller.observe(), Err(iommu::Error::Mode));
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
    retry(|| controller.shut_queue());
    unsafe { controller.begin_shut() }.unwrap();
    assert_eq!(retry(|| controller.observe_directory()), Directory::Off);
    assert_eq!(controller.retained_root(), Some(root));
    assert_eq!(
        unsafe { controller.begin_install(root) },
        Err(iommu::Error::Mode)
    );
    boot::write(
        "\nPASS riscv-iommu sv39=1 directory=1 replacement-rejected=1 off=1 fences=130 walk=1 fault=1 dma=1 downgrade=1 bindings=2\n",
    );
    boot::shutdown(true)
}
