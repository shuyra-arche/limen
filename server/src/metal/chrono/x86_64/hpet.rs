//! A retained local observation of a real HPET main counter.

use core::num::NonZeroU64;

const FEMTOSECONDS: u64 = 1_000_000_000_000_000;
const MAXIMUM_PERIOD: u64 = 100_000_000;

/// Decode the nominal whole-Hertz rate reported by the HPET hardware.
/// An unrepresentable fractional rate is not rounded into a trusted clock.
pub fn frequency(period: u32) -> Option<NonZeroU64> {
    let period = u64::from(period);
    if period == 0 || period > MAXIMUM_PERIOD || !FEMTOSECONDS.is_multiple_of(period) {
        return None;
    }
    NonZeroU64::new(FEMTOSECONDS / period)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Scope,
    Capability,
    Busy,
    Unconfirmed,
}

#[cfg(all(limen_target, target_arch = "x86_64"))]
pub use native::{Hpet, Prepared};

#[cfg(all(limen_target, target_arch = "x86_64"))]
mod native {
    use core::num::NonZeroU64;
    use limen_treaty::model::Alignment;

    use super::{Failure, frequency};
    use crate::metal::{
        chrono::treaty::Clock,
        memory::access::treaty::device::DeviceWindow,
        platform::description::treaty::acpi::Hpet as Description,
        processor::treaty::{ProcessorId, try_current_processor},
    };

    const ID: usize = 0x00;
    const PERIOD: usize = 0x04;
    const CONFIGURATION: usize = 0x10;
    const STATUS: usize = 0x20;
    const LOW: usize = 0xf0;
    const HIGH: usize = 0xf4;
    const ENABLED: u32 = 1;
    const LEGACY: u32 = 2;
    const WIDE: u32 = 1 << 13;
    const TIMER_BASE: usize = 0x100;
    const TIMER_STRIDE: usize = 0x20;
    const TIMER_IRQ: u32 = 1 << 2;
    const TIMER_FSB: u32 = 1 << 14;

    /// The pre-admitted owner retains the original mapping and any issued
    /// enable effect until the actual configuration is observed.
    #[must_use]
    pub struct Prepared {
        window: DeviceWindow<'static>,
        processor: ProcessorId,
        frequency: NonZeroU64,
        timers: usize,
        issued: bool,
    }

    /// A continuously enabled 64-bit HPET, without a writable comparator
    /// or another owner of its global configuration. Its original mapping
    /// stays with the actual Clock source, not in a global address registry.
    #[must_use]
    pub struct Hpet {
        clock: Clock,
    }

    impl Hpet {
        /// Inspect an exclusively handed-off HPET without issuing a write.
        ///
        /// # Safety
        /// The retained mapping names the actual HPET block with correct UC
        /// attributes. Firmware and all other software have relinquished its
        /// configuration, counter and timer control. The caller proves old
        /// interrupt obligations are retired. During this lifetime no agent
        /// may stop, reset, reprogram or independently admit this block,
        /// and the counter must continue through all permitted power states.
        /// The nominal rate is not a physical worst-case timing certificate.
        pub unsafe fn prepare(
            description: Description<'_>,
            window: DeviceWindow<'static>,
        ) -> Result<Prepared, (DeviceWindow<'static>, Failure)> {
            let Some(processor) = try_current_processor() else {
                return Err((window, Failure::Scope));
            };
            if window.physical() != description.registers()
                || !window.is_aligned(Alignment::new(4).expect("HPET alignment"))
                || !window.supports_u32(HIGH)
            {
                return Err((window, Failure::Scope));
            }
            let id = register(&window, ID);
            if id != description.block_id() || id & WIDE == 0 {
                return Err((window, Failure::Capability));
            }
            let Some(frequency) = frequency(register(&window, PERIOD)) else {
                return Err((window, Failure::Capability));
            };
            let timers = ((id >> 8) & 0x1f) as usize + 1;
            if !window.supports_u32(TIMER_BASE + (timers - 1) * TIMER_STRIDE) {
                return Err((window, Failure::Capability));
            }
            if !quiet(&window, timers) {
                return Err((window, Failure::Busy));
            }
            Ok(Prepared {
                window,
                processor,
                frequency,
                timers,
                issued: false,
            })
        }

        pub fn clock(&self) -> &Clock {
            &self.clock
        }
    }

    impl Prepared {
        /// Enable the original main counter once, without resetting it or
        /// modifying timer routing. Retain the original owner on uncertainty;
        /// retries after a write observe rather than repeat it.
        pub fn start(mut self) -> Result<Hpet, (Self, Failure)> {
            if try_current_processor() != Some(self.processor) {
                return Err((self, Failure::Scope));
            }
            if !quiet(&self.window, self.timers) {
                return Err((self, Failure::Busy));
            }
            if register(&self.window, CONFIGURATION) & ENABLED == 0 {
                if self.issued {
                    return Err((self, Failure::Unconfirmed));
                }
                self.issued = true;
                // Legacy replacement and each interrupt enable were checked.
                // Write only the defined general configuration enable bit.
                unsafe { self.window.write_u32(CONFIGURATION, ENABLED) }
                    .expect("checked HPET configuration");
            }
            if register(&self.window, CONFIGURATION) & (ENABLED | LEGACY) != ENABLED {
                return Err((self, Failure::Unconfirmed));
            }
            let Some(first) = observe(&self.window) else {
                return Err((self, Failure::Unconfirmed));
            };
            let mut advanced = false;
            for _ in 0..1024 {
                match observe(&self.window) {
                    Some(count) if count > first => {
                        advanced = true;
                        break;
                    }
                    Some(count) if count < first => return Err((self, Failure::Unconfirmed)),
                    _ => core::hint::spin_loop(),
                }
            }
            if !advanced {
                return Err((self, Failure::Unconfirmed));
            }
            match Clock::bind_mapped(self.processor, self.frequency, self.window, observe) {
                Ok(clock) => Ok(Hpet { clock }),
                Err(window) => {
                    self.window = window;
                    Err((self, Failure::Scope))
                }
            }
        }
    }

    fn register(window: &DeviceWindow<'_>, offset: usize) -> u32 {
        // Only checked, non-destructive HPET registers use this helper.
        unsafe { window.observe_u32(offset) }.expect("validated HPET geometry")
    }

    fn quiet(window: &DeviceWindow<'_>, timers: usize) -> bool {
        register(window, CONFIGURATION) & LEGACY == 0
            && register(window, STATUS) == 0
            && (0..timers).all(|timer| {
                register(window, TIMER_BASE + timer * TIMER_STRIDE) & (TIMER_IRQ | TIMER_FSB)
                    == 0
            })
    }

    fn observe(window: &DeviceWindow<'_>) -> Option<u64> {
        // A device is allowed to split a 64-bit MMIO read into two accesses.
        // Bound the high/low/high retry and reject an inconsistent result.
        if register(window, CONFIGURATION) & (ENABLED | LEGACY) != ENABLED {
            return None;
        }
        for _ in 0..4 {
            let high = register(window, HIGH);
            let low = register(window, LOW);
            if register(window, HIGH) == high
                && register(window, CONFIGURATION) & (ENABLED | LEGACY) == ENABLED
            {
                return Some((u64::from(high) << 32) | u64::from(low));
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "../../../../tests/metal/chrono/hpet.rs"]
mod tests;
