#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

use core::{
    arch::asm,
    sync::atomic::{AtomicUsize, Ordering},
};
#[cfg(target_arch = "riscv64")]
use kernel::metal::processor::treaty::Startup;
use kernel::{
    entry::boot::Boot,
    metal::{
        platform::boot::treaty as diagnostic,
        processor::treaty::{LocalStorage, Minor, Stack},
        sync::treaty::Supply,
    },
};

#[cfg(target_os = "uefi")]
fn prepare() -> Result<(), uefi::Status> {
    #[cfg(target_arch = "x86_64")]
    if matches!(case(), "minor-pass" | "minor-panic") {
        uefi_fixture::q35::prepare_minor()?;
    }
    Ok(())
}

static MINOR_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static MINOR_LOCAL: LocalStorage<5> = LocalStorage::new(Supply::new());
static MINOR: Minor = Minor::new(&MINOR_STACK, &MINOR_LOCAL);
static COMPLETED: AtomicUsize = AtomicUsize::new(usize::MAX);

fn case() -> &'static str {
    option_env!("LIMEN_TEST_CASE").unwrap_or("pass")
}

fn minor(hart: usize, major: usize) -> ! {
    assert_ne!(hart, major);
    assert!(MINOR.ready());
    #[cfg(target_os = "uefi")]
    uefi_fixture::check_minor(&MINOR, hart);

    if case() == "minor-panic" {
        panic!("T0 intentional minor assertion failure");
    }
    COMPLETED.store(hart, Ordering::Release);
    loop {
        core::hint::spin_loop();
    }
}

fn check_minor(boot: &Boot<'_>) {
    let major = boot.hart();
    #[cfg(target_arch = "riscv64")]
    let (target, mut startup) = {
        let target = (0..4).find(|hart| *hart != major).unwrap();
        let harts = boot
            .sbi_services()
            .hart_state_management()
            .unwrap()
            .unwrap();
        (target, Startup::Sbi(harts))
    };
    #[cfg(target_os = "uefi")]
    let (target, mut startup) = {
        let map = uefi_fixture::check_handoff(boot, Some(&MINOR));
        (uefi_fixture::target(), uefi_fixture::inspect(boot, &map))
    };
    unsafe { MINOR.start(target, minor, major, &mut startup) }.expect("T0 minor startup");
    // A failing minor never publishes completion; the major cannot pass.
    while COMPLETED.load(Ordering::Acquire) != target {
        core::hint::spin_loop();
    }
    assert!(MINOR.ready());
    #[cfg(target_os = "uefi")]
    uefi_fixture::inspect_started(&startup, target);
}

// Test-only reset requests deliberately bypass ordinary shutdown.
fn reset(_boot: &Boot<'_>) -> ! {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        // SBI SRST, cold reboot, no reason.
        asm!(
            "ecall",
            inlateout("a0") 1_usize => _,
            inlateout("a1") 0_usize => _,
            in("a6") 0_usize,
            in("a7") 0x53525354_usize,
            options(nostack)
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        use kernel::metal::platform::firmware::treaty::psci::{Conduit, PsciDescription};
        let tree = _boot.device_tree_description().unwrap();
        let description = PsciDescription::read(&tree).unwrap().unwrap();
        // PSCI SYSTEM_RESET using the actual verified firmware conduit.
        unsafe {
            description.verify().unwrap();
            match description.conduit() {
                Conduit::Hvc => asm!(
                    "hvc #0",
                    inlateout("x0") 0x84000009_usize => _,
                    clobber_abi("C")
                ),
                Conduit::Smc => asm!(
                    "smc #0",
                    inlateout("x0") 0x84000009_usize => _,
                    clobber_abi("C")
                ),
            }
        }
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // q35 reset control register.
        asm!("out dx, al", in("dx") 0xcf9_u16, in("al") 6_u8, options(nostack));
    }
    panic!("T0 reset request returned");
}

fn check(boot: Boot<'_>) {
    match case() {
        "pass" => assert_ne!(boot.hart(), usize::MAX),
        "panic" => assert_eq!(boot.hart(), usize::MAX, "T0 intentional assertion failure"),
        "minor-pass" | "minor-panic" => check_minor(&boot),
        "reset" => reset(&boot),
        "shutdown" => diagnostic::shutdown(true),
        "fake-log" => {
            diagnostic::write("PASS\n");
            diagnostic::shutdown(true);
        }
        "trap" => {
            unsafe {
                #[cfg(target_arch = "x86_64")]
                asm!("ud2", options(nostack));
                #[cfg(target_arch = "aarch64")]
                asm!("udf #0", options(nostack));
                #[cfg(target_arch = "riscv64")]
                asm!(".4byte 0", options(nostack));
            }
            panic!("T0 illegal instruction returned");
        }
        "timeout" => loop {
            core::hint::spin_loop();
        },
        "discovery-panic" => panic!("compiler collection missed the nested case"),
        _ => panic!("unknown T0 case"),
    }
}

guest::entry! {
    platform: platform::Platform::native(4),
    prepare: prepare,
    run: |boot| {
        check(boot);
        guest::finish()
    }
}

// This fixture tests the compiler-collected harness itself. No ordinary
// hardware contract has to contain a synthetic discovery probe.
mod collection {
    #[test_case]
    fn admitted_local_state_is_available() {
        assert_eq!(kernel::metal::sync::treaty::turn_capacity(), Some(5));
        if super::case() == "discovery-panic" {
            panic!("intentional nested collected test failure");
        }
    }
}
