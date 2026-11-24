/// Firmware descriptor bytes, with the stride and version from the final UEFI map.
#[derive(Clone, Copy)]
pub struct FirmwareMap<'a> {
    pub descriptors: &'a [u8],
    pub stride: usize,
    pub version: u32,
}
