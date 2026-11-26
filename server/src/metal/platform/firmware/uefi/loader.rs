use limen_treaty::model::{PhysicalAddress, PhysicalRange};

/// How the trusted platform uses its retained DTB.
pub enum DeviceTree<'a> {
    /// An immutable hardware description retained for device discovery.
    Description(&'a [u8]),
    /// A memory description to check against the final UEFI map.
    Memory(&'a [u8]),
}

/// An ACPI configuration-table location and independent trusted read access.
pub struct AcpiInput<'a> {
    pub rsdp: PhysicalAddress,
    /// Return exactly the requested stable bytes from this physical range.
    /// Keep the original storage reserved throughout the returned borrow.
    pub read: &'a dyn Fn(PhysicalRange) -> Option<&'a [u8]>,
}

/// Loader-owned storage retained across the stack switch and Boot delivery.
/// The optional DTB is the exact, immutable copy in loader-data pages.
pub struct Loader<'a> {
    /// Physical extent of the resident image under the entry identity map.
    pub image: PhysicalRange,
    pub descriptors: &'a [u8],
    pub stride: usize,
    pub version: u32,
    pub descriptor_limit: usize,
    pub device_tree: Option<DeviceTree<'a>>,
    pub acpi: Option<AcpiInput<'a>>,
}
