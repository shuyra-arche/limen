use core::arch::naked_asm;

/// Test-owned reception window. The production bootstrap vector remains
/// terminal. This leaf uses only caller-clobbered registers; the trap lands
/// exclusively inside this window, then restores the original vector with
/// global interrupts masked. The actual IRQ backend runs after restoration.
#[unsafe(naked)]
pub(super) unsafe extern "C" fn receive(until: u64) -> usize {
    naked_asm!(
        ".option push",
        ".option norvc",
        "csrr t3, stvec",
        "lla t0, 3f",
        "csrw stvec, t0",
        "csrsi sstatus, 2",
        "4: rdtime t0",
        "bltu t0, a0, 4b",
        "csrci sstatus, 2",
        "li a0, 0",
        "j 2f",
        ".balign 4",
        "3: csrr a0, scause",
        "lla t0, 2f",
        "csrw sepc, t0",
        "li t0, 32",
        "csrc sstatus, t0",
        "sret",
        "2: csrci sstatus, 2",
        "csrw stvec, t3",
        "ret",
        ".option pop",
    )
}
