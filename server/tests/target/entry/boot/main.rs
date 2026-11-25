#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::fmt::Write;
use core::panic::PanicInfo;
use kernel::entry::boot::{Boot, Physical, Source, shutdown, stack_range, write};

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
    assert_eq!(boot.image(), Physical { start: image.start as u64, end: image.end as u64 });
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
    // bounded span. Verify that the witness names the actual virt RAM blob.
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

    let memory = boot.dtb_memory().expect("OpenSBI DTB memory");
    assert!(boot.conventional_ram().is_none());
    let early = boot.exclusions();
    assert!(early.entries().any(|entry| entry.occupant == kernel::entry::boot::Occupant::MajorStack
        && entry.physical.start <= sp as u64 && (sp as u64) < entry.physical.end));
    assert!(early.entries().any(|entry| entry.occupant == kernel::entry::boot::Occupant::TrapStack
        && boot.image().start <= entry.physical.start && entry.physical.end <= boot.image().end));
    assert!(early.entries().any(|entry| entry.occupant == kernel::entry::boot::Occupant::DeviceTree
        && entry.physical.start == device_tree.start as u64 && entry.physical.end == device_tree.end as u64));
    assert!(early.retains(memory.ram()[0], None).unwrap());
    assert!(early.entries().any(|entry| matches!(entry.occupant, kernel::entry::boot::Occupant::Reservation(_))));
    assert_eq!(
        memory.ram(),
        &[Physical {
            start: 0x8000_0000,
            end: 0x8800_0000
        }]
    );
    assert!(
        memory.reservations().iter().any(|entry| {
            entry.physical.start == 0x8000_0000
                && entry.physical.end > entry.physical.start
                && entry.physical.end <= 0x8020_0000
                && matches!(entry.source, Source::ReservedMemory { .. })
        }),
        "OpenSBI firmware reservation is missing"
    );

    write(
        "PASS boot-handoff firmware-hart=1 bare=1 masked=1 trap=1 stacks=1 bss=1 fdt=1 ram=1 reserved=1 exclusions=1\n",
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
