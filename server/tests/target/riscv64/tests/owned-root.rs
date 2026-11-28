#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

#[path = "../../../metal/memory/mmu/riscv64/internal/owned.rs"]
mod native;

guest::entry! {
    platform: platform::Platform::native(1),
    // Complete owned-root construction, two-page Mapping and retirement
    // share this image-owned startup stack, including Debug call frames.
    major: (64 * 1024, 5),
    test: |boot| {
        native::check(&boot);
    },
}
