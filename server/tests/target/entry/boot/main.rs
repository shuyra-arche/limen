#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::panic::PanicInfo;
use kernel::metal::{
    chrono::riscv::RiscvTimebase,
    processor::execution::description::riscv::RiscvExtension, processor::execution::description::riscv::RiscvIsa,
    platform::description::devicetree::memory::Source, processor::hart::riscv::description::RiscvCpus,
    memory::mmu::internal::riscv::description::{MaximumMode, RiscvMmu},
};
use kernel::entry::boot::{Boot, shutdown, stack_range, write};
use limen_treaty::model::{ByteLength, PhysicalAddress};

#[path = "../synchronization.rs"]
mod synchronization;

mod allocator;
#[path = "../foundation.rs"]
mod foundation;
#[path = "../memory.rs"]
mod memory;
mod pool;
#[path = "../pool.rs"]
mod pool_material;

unsafe extern "C" {
    static limen_test_text_start: u8;
    static limen_test_text_end: u8;
    static _kernel_base: u8;
    static _kernel_edge: u8;
    static _bss_start: u8;
    static _bss_end: u8;
}

// Nonzero initialization places the firmware witness outside the cleared BSS.
static mut FIRMWARE_HART: usize = usize::MAX;
static mut ZEROED: usize = 0;

// Firmware-facing test prelude. Preserve a0/a1 and do not assume gp or sp.
global_asm!(
    ".pushsection .limen_test_entry,\"ax\",@progbits",
    ".balign 4",
    ".globl limen_test_entry",
    "limen_test_entry:",
    ".option push",
    ".option norelax",
    "lla t0, {hart}",
    "sd a0, 0(t0)",
    "lla t0, {sentinel}",
    "li t1, -1",
    "sd t1, 0(t0)",
    "tail _start",
    ".option pop",
    ".popsection",
    hart = sym FIRMWARE_HART,
    sentinel = sym ZEROED,
);

fn check_discovery(boot: &Boot<'_>) {
    let tree = boot.device_tree_description().expect("OpenSBI source tree");
    let cpus = RiscvCpus::new(tree).unwrap().expect("QEMU CPU scope");
    let hart = boot.hart() as u64;
    let cpu = cpus.cpu(hart).unwrap().expect("actual Hart description");
    assert_eq!(cpu.id(), hart);
    assert!(cpu.enabled());
    assert!(cpus.cpu(u64::MAX).unwrap().is_none());

    let mmu = RiscvMmu::describe(&cpu)
        .unwrap()
        .expect("declared MMU mode");
    assert!(matches!(
        mmu.maximum(),
        MaximumMode::Paged(_)
    ));
    assert_eq!(mmu.source(), cpu.node().id());
    let isa = RiscvIsa::describe(&cpu).unwrap().expect("declared ISA");
    assert_eq!(isa.source(), cpu.node().id());
    for extension in [
        RiscvExtension::Integer,
        RiscvExtension::Atomic,
        RiscvExtension::Float,
        RiscvExtension::Double,
        RiscvExtension::Compressed,
    ] {
        assert!(isa.has(extension));
    }

    let timebase = RiscvTimebase::describe(&cpus)
        .unwrap()
        .expect("QEMU timebase");
    assert_eq!(timebase.source(), cpus.node().id());
    assert_eq!(timebase.hertz(), 10_000_000);
    assert!(boot.acpi().is_none(), "SBI supplied a DTB, not ACPI");
    let sbi = kernel::metal::platform::firmware::sbi::platform_services();
    assert!(sbi.hart_state_management() && sbi.timer() && sbi.inter_processor_interrupt());
    write("PASS firmware-discovery rv64 hart=1 mmu=1 isa=1 timebase=1 sbi=1\n");
}

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(boot: Boot) -> ! {
    let original = unsafe { (&raw const FIRMWARE_HART).read_volatile() };
    assert!(
        original < 4 && boot.hart() == original,
        "firmware hart ID changed"
    );

    let sp: usize;
    let satp: usize;
    let sstatus: usize;
    let sie: usize;
    let stvec: usize;
    let sscratch: usize;
    // The callback runs in supervisor mode after the actual assembly entry.
    unsafe {
        asm!(
            "mv {sp}, sp",
            "csrr {satp}, satp",
            "csrr {sstatus}, sstatus",
            "csrr {sie}, sie",
            "csrr {stvec}, stvec",
            "csrr {sscratch}, sscratch",
            sp = out(reg) sp,
            satp = out(reg) satp,
            sstatus = out(reg) sstatus,
            sie = out(reg) sie,
            stvec = out(reg) stvec,
            sscratch = out(reg) sscratch,
            options(nomem, nostack),
        );
    }
    assert!(satp == 0, "translation enabled before the handoff");
    assert!(sstatus & 2 == 0 && sie == 0, "interrupt delivery enabled");
    let code = (&raw const limen_test_text_start).addr()..(&raw const limen_test_text_end).addr();
    assert!(
        code.contains(&stvec) && stvec.is_multiple_of(4),
        "direct early trap vector missing",
    );
    let image = (&raw const _kernel_base).addr()..(&raw const _kernel_edge).addr();
    assert_eq!(
        boot.image(),
        memory::extent(image.start as u64, image.end as u64)
    );
    let bss = (&raw const _bss_start).addr()..(&raw const _bss_end).addr();
    assert!(
        sscratch.is_multiple_of(8)
            && image.contains(&sscratch)
            && sscratch.checked_add(8).is_some_and(|end| {
                end <= image.end && (end <= bss.start || sscratch >= bss.end)
            }),
        "emergency stack claim outside initialized image storage"
    );
    let top: usize;
    unsafe { asm!("ld {}, 0({})", out(reg) top, in(reg) sscratch, options(nostack, readonly)) };
    let reserved = stack_range();
    let emergency = top.checked_sub(16 * 1024).expect("emergency stack top");
    assert!(
        top.is_multiple_of(16)
            && image.contains(&emergency)
            && top <= image.end
            && (top <= reserved.start || emergency >= reserved.end)
            && (top <= sscratch || emergency >= sscratch + 8),
        "emergency stack is unarmed or overlaps live storage",
    );
    assert!(
        reserved.len() == 32 * 1024
            && image.contains(&reserved.start)
            && reserved.end <= image.end
            && reserved.start.is_multiple_of(16)
            && reserved.contains(&sp)
            && sp.is_multiple_of(16),
        "invalid major stack reservation or supervisor stack pointer",
    );

    let zeroed = &raw const ZEROED;
    assert!(
        bss.contains(&zeroed.addr()) && unsafe { zeroed.read_volatile() } == 0,
        "zero-initialized storage missing",
    );

    // The production handoff has already checked the firmware header and
    // bounded extent. Verify that the witness names the actual virt RAM blob.
    assert!(boot.firmware_map().is_none());
    let device_tree = boot.device_tree().expect("OpenSBI DTB");
    assert!(
        device_tree.start >= 0x8000_0000 && device_tree.end <= 0x8800_0000,
        "firmware tree is outside QEMU virt RAM",
    );
    assert!(
        device_tree.end >= device_tree.start + 40 && device_tree.start.is_multiple_of(8),
        "invalid firmware tree extent",
    );
    let header = device_tree.start as *const u32;
    let magic = u32::from_be(unsafe { header.read_volatile() });
    let length = u32::from_be(unsafe { header.add(1).read_volatile() }) as usize;
    assert!(magic == 0xd00d_feed, "invalid firmware tree magic");
    assert!(length == device_tree.len(), "firmware tree length mismatch");
    check_discovery(&boot);

    let memory = boot.dtb_memory().expect("OpenSBI DTB memory");
    assert!(boot.conventional_ram().is_none());
    let early = boot.exclusions();
    assert!(early.entries().any(|entry| entry.occupant
        == kernel::entry::boot::Occupant::MajorStack
        && entry.physical.contains(PhysicalAddress::new(sp as u64))));
    assert!(early.entries().any(|entry| entry.occupant
        == kernel::entry::boot::Occupant::TrapStack
        && boot.image().covers(entry.physical)));
    assert!(early.entries().any(|entry| entry.occupant
        == kernel::entry::boot::Occupant::DeviceTree
        && entry.physical == memory::extent(device_tree.start as u64, device_tree.end as u64)));
    let bank = memory::extent(0x8000_0000, 0x8800_0000);
    assert!(memory.ram().eq([bank]));
    assert!(early.retains(bank, None));
    let mut storage = [None; 4];
    let mut regions = boot
        .regions(ByteLength::new(4096), None, &mut storage)
        .unwrap();
    assert!(regions.entries().next().is_none());
    assert!(
        regions
            .reserve_root(&mut None, core::alloc::Layout::new::<u64>())
            .unwrap()
            .is_none()
    );
    assert!(early.entries().any(|entry| matches!(
        entry.occupant,
        kernel::entry::boot::Occupant::Reservation(_)
    )));
    assert!(
        memory.reservations().any(|entry| {
            entry.physical.start().get() == 0x8000_0000
                && entry.physical.end().get() <= 0x8020_0000
                && matches!(entry.source, Source::ReservedMemory { .. })
        }),
        "OpenSBI firmware reservation is missing"
    );

    allocator::check(&boot);
    pool::check(&boot);
    let foundation = memory::extent(0x8430_0000, 0x8430_0000 + 4096 * foundation::PAGES as u64);
    let evidence = memory::retirement(&boot, foundation);
    unsafe { foundation::check(&boot, foundation.start().get(), &evidence) };
    synchronization::check();
    synchronization::check_enabled();
    write("PASS synchronization\n");
    write(
        "PASS boot-handoff firmware-hart=1 bare=1 masked=1 trap=1 stacks=1 bss=1 fdt=1 ram=1 reserved=1 exclusions=1 regions=0 roots=0\n",
    );
    shutdown(true)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if let Some(mut console) = kernel::entry::boot::console() {
        let _ = writeln!(console, "FAIL boot-handoff: {info}");
    }
    shutdown(false)
}
