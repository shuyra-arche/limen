#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(guest::run)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

use kernel::metal::sync::treaty::turn_capacity;

guest::entry! {
    platform: platform::Platform::native(4),
    run: |mut boot| {
        assert_ne!(boot.hart(), usize::MAX);
        assert!(!boot.stack_range().is_empty());
        let _major = boot.take_major().expect("original major ownership");
        assert!(boot.take_major().is_none());
        collected();
        guest::finish()
    }
}

#[test_case]
fn installed_local_state_is_available() {
    assert_eq!(turn_capacity(), Some(5));
}
