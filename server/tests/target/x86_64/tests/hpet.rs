#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(harness)]
#![reexport_test_harness_main = "collected"]

use core::{hint::spin_loop, time::Duration};
use guest::platform;
use kernel::metal::{
    chrono::x86_64::hpet::Hpet,
    memory::access::treaty::device::DeviceWindow,
    platform::boot::treaty::write,
};
use limen_treaty::model::InternalAddress;

#[path = "../../../metal/chrono/observation.rs"]
mod observation;

guest::entry! {
    platform: platform::Platform::native(1),
    test: |boot| {
        let description = boot.acpi().unwrap().hpet().unwrap().expect("original HPET");
        let range = description.registers();
        // The q35 fixture retains the early identity device mapping with
        // UC attributes. Firmware has handed the actual block to this guest.
        // The implementation still rejects legacy routing, enabled timer
        // IRQs and pending sources before any write.
        let window = unsafe {
            DeviceWindow::admit(range, InternalAddress::new(range.start().get()))
        }.unwrap();
        let prepared = unsafe { Hpet::prepare(description, window) }
            .unwrap_or_else(|(_, reason)| panic!("HPET prepare: {reason:?}"));
        let hpet = prepared.start()
            .unwrap_or_else(|(_, reason)| panic!("HPET start: {reason:?}"));
        let clock = hpet.clock();
        assert_eq!(clock.processor(), boot.processor());
        assert_eq!(clock.frequency().get(), 100_000_000);
        observation::observe(clock);
        let initial = clock.sample().unwrap();
        let duration = Duration::from_millis(1);
        assert!(initial.after(duration).is_some());
        for _ in 0..200_000 {
            let now = clock.sample().expect("original mapped HPET");
            if now.elapsed_since(initial).unwrap() >= duration {
                write("PASS hpet original-acpi=1 native-counter=1 rate=1 elapsed=1\n");
                return;
            }
            spin_loop();
        }
        panic!("HPET did not advance");
    },
}
