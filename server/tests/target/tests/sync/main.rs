#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use guest::platform;

guest::entry! {
    platform: platform::Platform::native(4),

    test: {
        collected;
        mod contracts;
    },

    #[cfg(target_arch = "riscv64")]
    bench: {
        measurement::check;
        #[path = "../../../../benches/metal/sync/shared/mod.rs"]
        mod measurement;
    },
}
