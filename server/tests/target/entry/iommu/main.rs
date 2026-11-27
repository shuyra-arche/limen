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
static PAYLOAD: Page = Page([0; 512]);
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
    // The VM has one IOMMU, no endpoints, no PCI ATS, and Bare SATP. Its
    // coherent RAM contains a permanent zeroed DDT with every context invalid.
    let mut window =
        unsafe { DeviceWindow::admit(range, InternalAddress::new(range.start().get())).unwrap() };
    let profile = unsafe { riscv::probe(&mut window) }.unwrap();
    assert!(profile.sv39 && profile.debug_translation);
    confirm(
        &mut unsafe { riscv::shut(&mut window, order) }.unwrap(),
        Directory::Off,
    );
    // First stage maps IOVA 0x1000 read-only to a different physical page.
    let tables = unsafe { &mut *(&raw mut TABLES) };
    tables[0].0[0] = profile.branch(physical(&tables[1])).unwrap();
    tables[1].0[0] = profile.branch(physical(&tables[2])).unwrap();
    tables[2].0[1] = profile
        .leaf(physical(&PAYLOAD), riscv::translation::Access::Read)
        .unwrap();
    let directory = unsafe { &mut *(&raw mut DIRECTORY) };
    directory.0[..4].copy_from_slice(&profile.context(physical(&tables[0]), 1).unwrap());
    let root = physical(directory);
    confirm(
        &mut unsafe { riscv::install(&mut window, root, order) }.unwrap(),
        Directory::One,
    );
    assert_eq!(
        translate(&mut window, 0x1000, true),
        Some(physical(&PAYLOAD).get())
    );
    assert_eq!(translate(&mut window, 0x2000, true), None);
    assert!(matches!(
        unsafe { riscv::install(&mut window, root, order) },
        Err(riscv::Error::Mode)
    ));
    confirm(
        &mut unsafe { riscv::shut(&mut window, order) }.unwrap(),
        Directory::Off,
    );
    // Even Off can retain cached DDT entries and translations. Invalidate
    // both on the same controller and consume each batch's own fence.
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
            Invalidate::Device(0)
        } else {
            Invalidate::FirstStage
        };
        retry(|| queue.submit(operation));
        assert_eq!(queue.submit(operation), Err(riscv::Error::Mode));
        assert_eq!(retry(|| queue.observe()), operation);
        assert_eq!(queue.observe(), Err(riscv::Error::Mode));
    }
    retry(|| queue.shut());
    boot::write(
        "\nPASS riscv-iommu sv39=1 directory=1 replacement-rejected=1 off=1 fences=130 walk=1 fault=1\n",
    );
    boot::shutdown(true)
}
