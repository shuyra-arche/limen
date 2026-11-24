#![no_std]
#![no_main]

#[cfg(target_os = "uefi")]
mod q35;
#[cfg(target_arch = "aarch64")]
mod virt;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
