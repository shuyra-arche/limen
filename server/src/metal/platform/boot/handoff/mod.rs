//! Facts retained from the firmware handoff.

#[cfg_attr(
    not(any(limen_firmware = "sbi", test)),
    expect(dead_code, reason = "the SBI handoff supplies the original DTB window")
)]
pub(crate) mod sbi;
#[cfg(any(limen_firmware = "uefi", test))]
pub(crate) mod uefi;

use crate::metal::platform::{
    description::{
        acpi::Acpi,
        devicetree::{self, memory::DtbMemory},
    },
    firmware::uefi::map::FirmwareMap,
};
use limen_treaty::model::PhysicalRange;

#[cfg(any(limen_target, test))]
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg_attr(
    not(any(limen_target, test)),
    expect(dead_code, reason = "native entry consumes the admitted image and map")
)]
pub(crate) struct Facts<'a> {
    pub(crate) image: PhysicalRange,
    pub(crate) firmware_map: Option<FirmwareMap<'a>>,
    #[cfg(limen_target)]
    pub(crate) map_storage: Option<PhysicalRange>,
}

/// Parsed firmware descriptions with the lifetime of their original inputs.
pub(crate) struct Descriptions<'a> {
    #[cfg(limen_firmware = "sbi")]
    pub(crate) cpus: Option<crate::metal::processor::treaty::RiscvCpus<'a>>,
    #[cfg(limen_firmware = "sbi")]
    pub(crate) sbi: Option<crate::metal::platform::firmware::treaty::sbi::SbiServices>,
    pub(crate) device_tree: Option<devicetree::DeviceTree<'a>>,
    pub(crate) device_tree_physical: Option<PhysicalRange>,
    pub(crate) memory: Option<DtbMemory>,
    // The parser bounds its retained table directory; a missing description
    // is a runtime fact, not a property of the selected ISA.
    pub(crate) acpi: Option<Acpi<'a>>,
}

impl Descriptions<'_> {
    #[cfg_attr(
        not(any(limen_target, test)),
        expect(
            dead_code,
            reason = "native entry prepares the original descriptions at handoff"
        )
    )]
    pub(crate) const fn new() -> Self {
        Self {
            #[cfg(limen_firmware = "sbi")]
            cpus: None,
            #[cfg(limen_firmware = "sbi")]
            sbi: None,
            device_tree: None,
            device_tree_physical: None,
            memory: None,
            acpi: None,
        }
    }
}

/// Major-image storage for the retained parsed descriptions.
#[cfg(any(limen_target, test))]
pub(super) struct DescriptionStorage {
    claimed: AtomicBool,
    value: UnsafeCell<MaybeUninit<Descriptions<'static>>>,
}

// Exactly one claimant initializes the storage. No operation can mutate
// the descriptions after a borrow has been handed to the consumer.
#[cfg(any(limen_target, test))]
unsafe impl Sync for DescriptionStorage {}

#[cfg(any(limen_target, test))]
impl DescriptionStorage {
    pub(super) const fn new() -> Self {
        Self {
            claimed: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[cfg(limen_target)]
    pub(super) fn range(&self) -> core::ops::Range<usize> {
        let start = core::ptr::from_ref(self);
        start.addr()..start.wrapping_add(1).addr()
    }

    pub(super) fn retain<'a, T>(
        &'static self,
        prepare: impl FnOnce(&mut Descriptions<'a>) -> Option<T>,
    ) -> Option<(T, &'a Descriptions<'a>)> {
        if self.claimed.swap(true, Ordering::Relaxed) {
            return None;
        }

        // Only the slot has static residence. Interpret the same storage
        // with the original firmware borrows selected by this preparation,
        // and never access it as a Descriptions<'static> afterward.
        let value = self.value.get().cast::<Descriptions<'a>>();
        unsafe { value.write(Descriptions::new()) };
        let result = prepare(unsafe { &mut *value })?;
        Some((result, unsafe { &*value }))
    }
}

#[cfg(test)]
#[path = "../../../../../tests/metal/platform/boot/handoff/storage.rs"]
mod tests;

// The boot protocol's trusted entry mapping establishes the physical
// correspondence. Callers must use the original identity-mapped extent.
#[cfg(any(limen_target, test))]
pub(super) unsafe fn identity_extent(range: core::ops::Range<usize>) -> Option<PhysicalRange> {
    use limen_treaty::model::PhysicalAddress;

    PhysicalRange::new(
        PhysicalAddress::new(range.start as u64),
        PhysicalAddress::new(range.end as u64),
    )
}
