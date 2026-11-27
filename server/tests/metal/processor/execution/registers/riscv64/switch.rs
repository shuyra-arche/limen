//! The separate link-wrapped probe observes the production switch boundary.
use core::{
    arch::global_asm,
    sync::atomic::{AtomicUsize, Ordering},
};

global_asm!(include_str!("switch.S"));

#[unsafe(no_mangle)]
static limen_register_probe_migrated: AtomicUsize = AtomicUsize::new(0);

pub fn migrations() -> usize {
    limen_register_probe_migrated.load(Ordering::Relaxed)
}

#[unsafe(no_mangle)]
extern "C" fn limen_register_probe_mismatch(register: usize) -> ! {
    panic!("kernel-execution-harts: register s{register} changed");
}
