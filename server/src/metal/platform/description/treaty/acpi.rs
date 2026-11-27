//! Select validated, unambiguous table bindings in one ACPI source.

use super::super::acpi::ivrs;

pub use super::super::acpi::{
    Acpi, Dmar, Error, IoApic, Ivhd, Madt, Polarity, Processor, Record, RemappingUnit,
    SourceOverride, Table, Trigger,
};

impl<'a> Acpi<'a> {
    pub fn dmar(&self) -> Result<Option<Dmar<'a>>, Error> {
        self.unique_v1_table(*b"DMAR")?
            .map(Dmar::validate)
            .transpose()
    }

    pub fn ivhd(&self) -> Result<Option<Ivhd<'a>>, Error> {
        self.unique_v1_table(*b"IVRS")?
            .map(|table| ivrs::decode(table.bytes()))
            .transpose()
    }

    fn unique_v1_table(&self, signature: [u8; 4]) -> Result<Option<Table<'a>>, Error> {
        let mut tables = self.tables().filter(|table| table.signature() == signature);
        let Some(table) = tables.next() else {
            return Ok(None);
        };
        if tables.next().is_some() {
            return Err(Error::DuplicateTable);
        }
        if table.revision() != 1 {
            return Err(Error::UnsupportedRevision);
        }
        Ok(Some(table))
    }
}
