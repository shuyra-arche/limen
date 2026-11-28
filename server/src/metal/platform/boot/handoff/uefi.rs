//! Trusted UEFI admission and retained description construction.

#[cfg(any(limen_firmware = "uefi", test))]
use super::identity_extent;
#[cfg(any(limen_firmware = "uefi", test))]
use super::{Descriptions, Facts};
#[cfg(any(limen_firmware = "uefi", test))]
use crate::metal::platform::{
    description::{acpi, devicetree},
    firmware::uefi::{DeviceTree, Loader, map::FirmwareMap},
};
#[cfg(limen_firmware = "uefi")]
use crate::metal::processor::treaty::Terminal;

#[cfg(any(limen_firmware = "uefi", test))]
const LOADER_DATA: u32 = 2;
#[cfg(any(limen_firmware = "uefi", test))]
const ACPI_RECLAIM: u32 = 9;
#[cfg(any(limen_firmware = "uefi", test))]
const ACPI_NVS: u32 = 10;

#[cfg(limen_firmware = "uefi")]
pub(crate) unsafe fn admit<'a>(
    processor: usize,
    observed: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    // The current loader handoff supplies an identity checked against the
    // physical CPU, rather than the trusted SBI entry's current-processor ABI.
    if processor != observed {
        Terminal::early().reject(format_args!(
            "FAIL Limen boot: major CPU identity mismatch\n"
        ));
    }
    // Read the loader once. Acpi::new consumes the read function and
    // retains only the original validated byte slices.
    unsafe { describe(input.cast::<Loader<'a>>().as_ref()?, descriptions) }
}

#[cfg(limen_firmware = "uefi")]
pub(crate) fn terminal_on_entry() -> Terminal {
    Terminal::early()
}

// The trusted loader supplies the original identity mapping for the retained
// descriptor and DTB buffers. Host parser tests provide that relation in fixtures.
#[cfg(any(limen_firmware = "uefi", test))]
pub(crate) unsafe fn describe<'a, 'b>(
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
    descriptions.device_tree_physical = None;
    descriptions.device_tree = match loader.device_tree.as_ref() {
        Some(tree) => {
            let bytes = match tree {
                DeviceTree::Description(bytes) | DeviceTree::Memory(bytes) => *bytes,
            };
            if !bytes.as_ptr().addr().is_multiple_of(8) {
                return None;
            }
            let pointers = bytes.as_ptr_range();
            let physical = unsafe { identity_extent(pointers.start.addr()..pointers.end.addr()) }?;
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
            descriptions.device_tree_physical = Some(physical);
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
    #[cfg(limen_firmware = "uefi")]
    let map_storage = {
        let bytes = loader.descriptors.as_ptr_range();
        unsafe { identity_extent(bytes.start.addr()..bytes.end.addr()) }?
    };
    Some(Facts {
        image: loader.image,
        firmware_map: Some(map),
        #[cfg(limen_firmware = "uefi")]
        map_storage: Some(map_storage),
    })
}
