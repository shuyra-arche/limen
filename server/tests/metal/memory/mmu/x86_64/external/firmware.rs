use crate::metal::platform::description::treaty::acpi::Acpi;
use limen_treaty::model::PhysicalAddress;

pub(super) struct Firmware<const N: usize> {
    rsdp: [u8; 36],
    xsdt: [u8; 44],
    table: [u8; N],
}

impl<const N: usize> Firmware<N> {
    pub(super) fn new(mut table: [u8; N]) -> Self {
        let mut rsdp = [0; 36];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&0x2000u64.to_le_bytes());
        checksum(&mut rsdp[..20], 8);
        checksum(&mut rsdp, 32);

        let mut xsdt = [0; 44];
        xsdt[..4].copy_from_slice(b"XSDT");
        xsdt[36..44].copy_from_slice(&0x3000u64.to_le_bytes());
        table_header(&mut xsdt);
        table_header(&mut table);
        Self { rsdp, xsdt, table }
    }

    pub(super) fn acpi(&self) -> Acpi<'_> {
        Acpi::new(PhysicalAddress::new(0x1000), |range| {
            let bytes: &[u8] = match range.start().get() {
                0x1000 => &self.rsdp,
                0x2000 => &self.xsdt,
                0x3000 => &self.table,
                _ => return None,
            };
            bytes.get(..range.length().get() as usize)
        })
        .unwrap()
    }
}

fn table_header(bytes: &mut [u8]) {
    let length = bytes.len() as u32;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes[8] = 1;
    checksum(bytes, 9);
}

fn checksum(bytes: &mut [u8], byte: usize) {
    bytes[byte] = bytes
        .iter()
        .copied()
        .fold(0u8, u8::wrapping_add)
        .wrapping_neg();
}
