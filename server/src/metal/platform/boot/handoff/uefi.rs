//! Trusted UEFI admission and retained description construction.

#[cfg(limen_firmware = "uefi")]
use super::super::treaty::Services;
use super::{Descriptions, Facts};
use crate::metal::platform::{
    description::{acpi, devicetree},
    firmware::uefi::{DeviceTree, Loader, map::FirmwareMap},
};
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

const LOADER_DATA: u32 = 2;
const ACPI_RECLAIM: u32 = 9;
const ACPI_NVS: u32 = 10;

#[cfg(limen_firmware = "uefi")]
pub(crate) unsafe fn admit<'a>(
    hart: usize,
    observed: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    // The current loader handoff supplies an identity checked against the
    // physical CPU, rather than the trusted SBI entry's current-Hart ABI.
    if hart != observed {
        Services::active().reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    // The trusted loader keeps this value and its buffers resident.
    describe(
        unsafe { input.cast::<Loader<'a>>().as_ref() }?,
        descriptions,
    )
}

#[cfg(limen_firmware = "uefi")]
pub(crate) fn services_on_entry() -> &'static Services {
    Services::active()
}

#[cfg(limen_firmware = "uefi")]
pub(crate) fn trap_services() -> &'static Services {
    Services::active()
}

pub(crate) fn describe<'a, 'b>(
    loader: &Loader<'a>,
    descriptions: &mut Descriptions<'b>,
) -> Option<Facts<'b>>
where
    'a: 'b,
{
    let map = FirmwareMap::new(
        loader.descriptors,
        loader.stride,
        loader.version,
        loader.descriptor_limit,
    )
    .ok()?;
    descriptions.device_tree = match loader.device_tree.as_ref() {
        Some(tree) => {
            let bytes = match tree {
                DeviceTree::Description(bytes) | DeviceTree::Memory(bytes) => *bytes,
            };
            if !bytes.as_ptr().addr().is_multiple_of(8) {
                return None;
            }
            let pointers = bytes.as_ptr_range();
            let physical = PhysicalRange::new(
                PhysicalAddress::new(pointers.start.addr() as u64),
                PhysicalAddress::new(pointers.end.addr() as u64),
            )?;
            if !map
                .regions()
                .any(|region| region.kind == LOADER_DATA && region.physical.covers(physical))
            {
                return None;
            }
            let description = devicetree::DeviceTree::new(bytes).ok()?;
            match tree {
                DeviceTree::Memory(_) => {
                    devicetree::memory::describe(description, &mut descriptions.memory)
                }
                DeviceTree::Description(_) => {
                    devicetree::memory::describe_reservations(description, &mut descriptions.memory)
                }
            }
            .ok()?;
            Some(description)
        }
        None => None,
    };
    if let Some(input) = &loader.acpi {
        descriptions.acpi = Some(
            acpi::Acpi::new(input.rsdp, |physical| {
                if !map.regions().any(|region| {
                    matches!(region.kind, ACPI_RECLAIM | ACPI_NVS)
                        && region.physical.covers(physical)
                }) {
                    return None;
                }
                (input.read)(physical)
            })
            .ok()?,
        );
    }
    Some(Facts {
        image: loader.image,
        firmware_map: Some(map),
    })
}
