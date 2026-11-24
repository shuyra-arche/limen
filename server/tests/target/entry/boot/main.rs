#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use kernel::entry::boot::{Boot, shutdown, stack_range, write};

unsafe extern "C" {
    static _boot_stack_start: u8;
    static _boot_stack_end: u8;
    static _trap_stack_start: u8;
    static _trap_stack_end: u8;
    static _limen_early_trap: u8;
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
    assert!(
        stvec == (&raw const _limen_early_trap).addr() && stvec.is_multiple_of(4),
        "direct early trap vector missing",
    );
    assert!(
        sscratch == (&raw const _trap_stack_end).addr()
            && sscratch > (&raw const _trap_stack_start).addr()
            && sscratch.is_multiple_of(16),
        "emergency stack not armed",
    );
    let reserved = (&raw const _boot_stack_start).addr()..(&raw const _boot_stack_end).addr();
    assert_eq!(stack_range(), reserved, "boot stack reservation mismatch");
    assert!(
        reserved.contains(&sp) && sp.is_multiple_of(16),
        "supervisor stack outside the boot stack",
    );

    let zeroed = &raw const ZEROED;
    assert!(
        ((&raw const _bss_start).addr()..(&raw const _bss_end).addr()).contains(&zeroed.addr())
            && unsafe { zeroed.read_volatile() } == 0,
        "zero-initialized storage missing",
    );

    // The production handoff has already checked the firmware header and
    // bounded span. Verify that the witness names the actual virt RAM blob.
    let device_tree = boot.device_tree();
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

    write("PASS boot-handoff firmware-hart=1 bare=1 masked=1 trap=1 stacks=1 bss=1 fdt=1\n");
    shutdown(true)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    write("FAIL boot-handoff: ");
    write(info.message().as_str().unwrap_or("assertion failed"));
    write("\n");
    shutdown(false)
}
