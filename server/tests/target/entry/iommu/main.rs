#![no_std]
#![no_main]

use core::{arch::asm, fmt::Write};
use kernel::{
    entry::boot::{self, Boot},
    metal::memory::mmu::{
        external::riscv::{self, Directory},
        internal::device::DeviceWindow,
    },
};
use limen_treaty::model::{InternalAddress, PhysicalAddress};

#[repr(align(4096))]
struct Page([u64; 512]);
static mut COMMANDS: riscv::queue::Page = riscv::queue::Page::EMPTY;
static mut DIRECTORY: Page = Page([0; 512]);
static mut TABLES: [Page; 3] = [const { Page([0; 512]) }; 3];
static mut PAYLOAD: Page = Page([0; 512]);
#[path = "../pci/edu.rs"]
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

fn confirm(change: &mut riscv::Change<'_>, expected: Directory) {
    for _ in 0..100_000 {
        match change.observe() {
            Err(riscv::Error::Busy) => core::hint::spin_loop(),
            result => {
                assert_eq!(result, Ok(expected));
                return;
            }
        }
    }
    panic!("DDTP write did not complete");
}

fn retry<T>(mut f: impl FnMut() -> Result<T, riscv::Error>) -> T {
    for _ in 0..100_000 {
        match f() {
            Ok(v) => return v,
            Err(riscv::Error::Busy) => core::hint::spin_loop(),
            Err(error) => panic!("queue failure: {error:?}"),
        }
    }
    panic!("queue operation did not complete")
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
        if let Some(registers) = riscv::registers(node).unwrap() {
            assert!(range.replace(registers).is_none());
        }
    }
    let range = range.expect("a platform IOMMU");
    // The VM has one quiesced EDU function, no PCI ATS, and Bare SATP.
    let hertz = kernel::metal::chrono::riscv::RiscvTimebase::describe(boot.riscv_cpus())
        .unwrap()
        .unwrap()
        .hertz();
    let mut edu = pci::Edu::open(&tree, range, true, order, ticks, u64::from(hertz));
    let mut window =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() };
    let profile = unsafe { riscv::probe(&mut window) }.unwrap();
    assert!(profile.sv39 && profile.debug_translation);
    confirm(
        &mut unsafe { riscv::shut(&mut window, order) }.unwrap(),
        Directory::Off,
    );
    // Give DID 0 and the firmware-selected EDU requester access to one page.
    const IOVA: u32 = 0x1000;
    const PATTERN: u32 = 0xbe71_6a29;
    let payload = &raw mut PAYLOAD;
    let data = payload.cast::<u32>();
    assert_ne!(u64::from(IOVA), physical(payload).get());
    let tables = unsafe { &mut *(&raw mut TABLES) };
    tables[0].0[0] = profile.branch(physical(&tables[1])).unwrap();
    tables[1].0[0] = profile.branch(physical(&tables[2])).unwrap();
    tables[2].0[1] = profile
        .leaf(physical(payload), riscv::translation::Access::ReadWrite)
        .unwrap();
    let directory = unsafe { &mut *(&raw mut DIRECTORY) };
    let context = profile.context(physical(&tables[0]), 1).unwrap();
    for did in [0, pci::RID] {
        let slot = profile.context_words(did).unwrap();
        directory.0[slot][..4].copy_from_slice(&context);
    }
    assert!(profile.context_words(128).is_err());
    let root = physical(directory);
    confirm(
        &mut unsafe { riscv::install(&mut window, root, order) }.unwrap(),
        Directory::One,
    );
    assert_eq!(
        translate(&mut window, 0x1000, true),
        Some(physical(payload).get())
    );
    assert_eq!(translate(&mut window, 0x2000, true), None);
    assert!(matches!(
        unsafe { riscv::install(&mut window, root, order) },
        Err(riscv::Error::Mode)
    ));
    unsafe { data.write_volatile(PATTERN) };
    order();
    edu.transfer(IOVA, false);
    unsafe { data.write_volatile(!PATTERN) };
    order();
    edu.transfer(IOVA, true);
    assert_eq!(unsafe { data.read_volatile() }, PATTERN);
    let leaf = profile
        .leaf(physical(payload), riscv::translation::Access::Read)
        .unwrap();
    unsafe { core::ptr::addr_of_mut!(tables[2].0[1]).write_volatile(leaf) };
    order();
    use riscv::queue::{Invalidate, Queue};
    let commands = unsafe { &mut *(&raw mut COMMANDS) };
    let address = PhysicalAddress::new(core::ptr::from_ref(commands).addr() as u64);
    let mut queue = unsafe { Queue::open(&mut window, commands, address, order) }.unwrap();
    assert_eq!(
        queue.submit(Invalidate::Device(1 << 24)),
        Err(riscv::Error::Address)
    );
    for i in 0..130 {
        let operation = if i % 2 == 0 {
            Invalidate::Device(pci::RID)
        } else {
            Invalidate::FirstStage
        };
        retry(|| queue.submit(operation));
        assert_eq!(queue.submit(operation), Err(riscv::Error::Mode));
        assert_eq!(retry(|| queue.observe()), operation);
        assert_eq!(queue.observe(), Err(riscv::Error::Mode));
    }
    unsafe { data.write_volatile(!PATTERN) };
    order();
    edu.transfer(IOVA, true);
    assert_eq!(unsafe { data.read_volatile() }, !PATTERN);
    retry(|| queue.shut());
    confirm(
        &mut unsafe { riscv::shut(&mut window, order) }.unwrap(),
        Directory::Off,
    );
    boot::write(
        "\nPASS riscv-iommu sv39=1 directory=1 replacement-rejected=1 off=1 fences=130 walk=1 fault=1 dma=1 downgrade=1\n",
    );
    boot::shutdown(true)
}
