//! Trusted SBI boot input and service installation.

#[cfg(limen_firmware = "sbi")]
use super::super::services::Services;
#[cfg(limen_firmware = "sbi")]
use super::{Descriptions, Facts};
use crate::metal::devicetree::{ALIGNMENT, HEADER_SIZE, MAX_BLOCK_BYTES, header_layout};
use crate::metal::platform::description::devicetree::{
    DeviceTree, Error,
    memory::{self, DtbMemory},
};
#[cfg(limen_firmware = "sbi")]
use crate::metal::{
    platform::firmware::sbi::{extensions, platform_services, reset, write_byte},
    processor::hart::riscv::description::RiscvCpus,
};
use core::{ops::Range, slice};
#[cfg(limen_firmware = "sbi")]
use limen_treaty::model::{PhysicalAddress, PhysicalRange};

#[cfg(limen_firmware = "sbi")]
static SERVICES: Services = Services {
    output: Some(write_byte),
    reset: Some(reset),
};

/// The trap vector is live before BSS has been cleared. Its failure path
/// probes firmware directly and never depends on a published service table.
#[cfg(limen_firmware = "sbi")]
pub(crate) fn trap_services() -> Services {
    let available = extensions();
    Services {
        output: SERVICES.output.filter(|_| available.console),
        reset: SERVICES.reset.filter(|_| available.reset),
    }
}

#[cfg(limen_firmware = "sbi")]
pub(crate) fn services_on_entry() -> &'static Services {
    let available = trap_services();
    if available.output.is_none() || available.reset.is_none() {
        available.reject(format_args!(
            "FAIL Limen boot: required SBI debug console or system reset unavailable\n"
        ));
    }
    SERVICES.install();
    &SERVICES
}

#[cfg(limen_firmware = "sbi")]
pub(crate) unsafe fn admit<'a>(
    hart: usize,
    input: *const (),
    descriptions: &mut Descriptions<'a>,
) -> Option<Facts<'a>> {
    // The image provides the readable physical window independently of the
    // a1 pointer or the bytes of the DTB. These symbols are addresses, not objects.
    unsafe extern "C" {
        static DTB_WINDOW_START: u8;
        static DTB_WINDOW_END: u8;
    }
    let window = (&raw const DTB_WINDOW_START).addr()..(&raw const DTB_WINDOW_END).addr();
    let image = crate::metal::platform::boot::image();
    let parsed = unsafe { read(input, window, image.clone(), &mut descriptions.memory) };
    let tree = match parsed {
        Ok(tree) => tree,
        Err(error) => Services::active().reject(format_args!("FAIL Limen boot: DTB {error:?}\n")),
    };
    let cpus = match RiscvCpus::new(tree) {
        Ok(Some(cpus)) => cpus,
        _ => Services::active().reject(format_args!("FAIL Limen boot: missing CPU scope\n")),
    };
    match cpus
        .cpu(hart as u64)
        .map(|cpu| cpu.is_some_and(|cpu| cpu.enabled()))
    {
        Ok(true) => {}
        Ok(false) => Services::active().reject(format_args!(
            "FAIL Limen boot: firmware Hart missing or disabled in DTB\n"
        )),
        Err(error) => {
            Services::active().reject(format_args!("FAIL Limen boot: DTB CPU {error:?}\n"))
        }
    }
    // The entry ABI supplies the current Hart identity in a0. HSM can also
    // report the state of that named Hart when the extension is available.
    let sbi = platform_services().unwrap_or_else(|error| {
        Services::active().reject(format_args!("FAIL Limen boot: SBI BASE {error:?}\n"))
    });
    let hsm = sbi.hart_state_management().unwrap_or_else(|error| {
        Services::active().reject(format_args!("FAIL Limen boot: SBI HSM probe {error:?}\n"))
    });
    if hsm.is_some_and(|service| !service.started(hart)) {
        Services::active().reject(format_args!("FAIL Limen boot: SBI Hart status mismatch\n"));
    }
    descriptions.cpus = Some(cpus);
    descriptions.sbi = Some(sbi);
    descriptions.device_tree = Some(tree);
    let image = PhysicalRange::new(
        PhysicalAddress::new(image.start as u64),
        PhysicalAddress::new(image.end as u64),
    )?;
    Some(Facts {
        image,
        firmware_map: None,
    })
}

/// # Safety
///
/// The trusted firmware must supply immutable readable storage for the header
/// at a nonzero aligned address. This check reads only the fixed-size header.
unsafe fn extent(header: *const [u32; 10], image: Range<usize>) -> Option<Range<usize>> {
    let address = header.addr();
    let header_end = address.checked_add(HEADER_SIZE)?;
    if header.is_null()
        || !address.is_multiple_of(ALIGNMENT)
        || overlaps_extent(&(address..header_end), &image)
    {
        return None;
    }

    let header = header_layout(unsafe { header.read_volatile() }.map(u32::from_be))?;
    let extent = address..address.checked_add(header.size as usize)?;
    (!overlaps_extent(&extent, &image)).then_some(extent)
}

fn overlaps_extent(first: &Range<usize>, second: &Range<usize>) -> bool {
    !first.is_empty() && !second.is_empty() && first.start < second.end && second.start < first.end
}

/// # Safety
///
/// The trusted platform provides a readable, identity-mapped window and
/// guarantees that the firmware's initialized DTB storage remains readable
/// and immutable for the returned view's lifetime. The DTB cannot supply any
/// part of that window bound.
pub(crate) unsafe fn read<'a>(
    input: *const (),
    window: Range<usize>,
    image: Range<usize>,
    memory: &mut Option<DtbMemory>,
) -> Result<DeviceTree<'a>, Error> {
    *memory = None;
    let start = input.addr();
    let header_end = start.checked_add(HEADER_SIZE).ok_or(Error::Window)?;
    if !window.contains(&start) || header_end > window.end {
        return Err(Error::Window);
    }
    let range = unsafe { extent(input.cast(), image) }.ok_or(Error::Format)?;
    if range.end > window.end {
        return Err(Error::Window);
    }
    if range.len() > MAX_BLOCK_BYTES {
        return Err(Error::Capacity);
    }
    // Only the exact DTB is borrowed. The enclosing window can contain
    // unrelated allocations, firmware storage and uninitialized bytes.
    let blob = unsafe { slice::from_raw_parts(input.cast(), range.len()) };
    let tree = DeviceTree::new(blob)?;
    memory::describe(tree, memory)?;
    Ok(tree)
}

#[cfg(test)]
#[path = "../../../../../tests/metal/platform/boot/handoff/sbi.rs"]
mod tests;
