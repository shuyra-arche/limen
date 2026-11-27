//! SBI admission for the shared cross-Hart migration scenario.

use kernel::{
    entry::boot::Boot,
    metal::processor::treaty::{Minor, Startup},
};

fn observe(minor: &Minor, _: usize) {
    let sp: usize;
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)) };
    assert!(minor.ready() && minor.stack_range().contains(&sp));
}

pub fn check(boot: Boot<'_>, complete: fn() -> !) -> ! {
    let target = (0..4)
        .filter(|hart| *hart != boot.processor())
        .find(|hart| {
            boot.riscv_cpus()
                .cpu(*hart as u64)
                .unwrap()
                .is_some_and(|cpu| cpu.enabled())
        })
        .expect("another enabled Hart");
    let service = boot
        .sbi_services()
        .hart_state_management()
        .unwrap()
        .expect("SBI HSM");
    super::run(
        boot,
        target,
        &mut Startup::Sbi(service),
        observe,
        |_| {},
        complete,
    )
}
