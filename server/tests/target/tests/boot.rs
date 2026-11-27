#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(support::run)]
#![reexport_test_harness_main = "collected"]

#[path = "../support/discovery/mod.rs"]
mod support;

use kernel::{entry::boot::Boot, metal::sync::treaty::turn_capacity};

#[unsafe(no_mangle)]
extern "Rust" fn limen_platform_ready(mut boot: Boot<'_>) -> ! {
    assert_ne!(boot.hart(), usize::MAX);
    assert!(!boot.stack_range().is_empty());
    let _major = boot.take_major().expect("original major ownership");
    assert!(boot.take_major().is_none());
    support::complete()
}

#[test_case]
fn installed_local_state_is_available() {
    assert_eq!(turn_capacity(), Some(5));
}
