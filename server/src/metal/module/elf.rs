//! Checked program headers and relative relocations for independent module images.

use super::Error;
use core::ops::Range;

const ELF_HEADER: usize = 64;
const PROGRAM_HEADER: usize = 56;
const DYNAMIC_ENTRY: usize = 16;
const RELA: usize = 24;
const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_HEADERS: usize = 32;
const MAX_NOTES: u64 = 8 * 1024;
const MAX_DYNAMIC: usize = 64;
const MAX_RELOCATIONS: usize = 512;

const DT_NULL: u64 = 0;
const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;
const DT_STRSZ: u64 = 10;
const DT_SYMENT: u64 = 11;
const DT_SYMBOLIC: u64 = 16;
const DT_FLAGS: u64 = 30;
const DT_GNU_HASH: u64 = 0x6fff_fef5;
const DT_RELACOUNT: u64 = 0x6fff_fff9;
const DF_SYMBOLIC: u64 = 2;

const LOAD: u32 = 1;
const DYNAMIC: u32 = 2;
const NOTE: u32 = 4;
const PHDR: u32 = 6;
const GNU_EH_FRAME: u32 = 0x6474_e550;
const GNU_STACK: u32 = 0x6474_e551;
const RISCV_ATTRIBUTES: u32 = 0x7000_0003;

const READ: u32 = 4;
const READ_WRITE: u32 = READ | 2;
const READ_EXECUTE: u32 = READ | 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Machine {
    Riscv64,
    X86_64,
    Aarch64,
}

impl Machine {
    fn number(self) -> u16 {
        match self {
            Self::Riscv64 => 243,
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
        }
    }

    fn relative(self) -> u64 {
        match self {
            Self::Riscv64 => 3,
            Self::X86_64 => 8,
            Self::Aarch64 => 1027,
        }
    }

    fn instruction(self, flags: u32) -> Result<u64, Error> {
        match self {
            // The current RV64 profile admits the ordinary integer register
            // file and standard floating-point ABI encodings.
            Self::Riscv64 if flags & !7 == 0 => Ok(if flags & 1 == 0 { 4 } else { 2 }),
            Self::X86_64 if flags == 0 => Ok(1),
            Self::Aarch64 if flags == 0 => Ok(4),
            _ => Err(Error::Unsupported),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permissions {
    ReadOnly,
    ReadWrite,
    ReadExecute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Segment<'a> {
    /// Link-time virtual address, independent of the placement selected at load.
    pub address: u64,
    pub memory_size: u64,
    pub file: &'a [u8],
    pub permissions: Permissions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Relocation {
    /// Link-time address of the full eight-byte write in a writable segment.
    pub address: u64,
    /// Link-time address of the referenced byte inside this image.
    pub addend: u64,
}

#[derive(Clone, Copy)]
struct Header {
    kind: u32,
    flags: u32,
    offset: u64,
    address: u64,
    file_size: u64,
    memory_size: u64,
    alignment: u64,
}

impl Header {
    fn read(bytes: &[u8; PROGRAM_HEADER]) -> Self {
        Self {
            kind: word(bytes, 0),
            flags: word(bytes, 4),
            offset: quad(bytes, 8),
            address: quad(bytes, 16),
            file_size: quad(bytes, 32),
            memory_size: quad(bytes, 40),
            alignment: quad(bytes, 48),
        }
    }

    fn file_range(self, bytes: &[u8]) -> Result<Range<usize>, Error> {
        let start = usize::try_from(self.offset).map_err(|_| Error::Range)?;
        let length = usize::try_from(self.file_size).map_err(|_| Error::Range)?;
        let end = start.checked_add(length).ok_or(Error::Range)?;
        (end <= bytes.len())
            .then_some(start..end)
            .ok_or(Error::Range)
    }

    fn memory(self) -> Result<Range<u64>, Error> {
        self.extent(self.memory_size)
    }

    fn extent(self, length: u64) -> Result<Range<u64>, Error> {
        Ok(self.address..self.address.checked_add(length).ok_or(Error::Range)?)
    }

    fn pages(self, page_size: u64) -> Result<Range<u64>, Error> {
        let memory = self.memory()?;
        let mask = page_size - 1;
        let end = memory
            .end
            .checked_next_multiple_of(page_size)
            .ok_or(Error::Range)?;
        Ok((memory.start & !mask)..end)
    }

    fn permissions(self) -> Result<Permissions, Error> {
        match self.flags {
            READ => Ok(Permissions::ReadOnly),
            READ_WRITE => Ok(Permissions::ReadWrite),
            READ_EXECUTE => Ok(Permissions::ReadExecute),
            _ => Err(Error::Unsupported),
        }
    }
}

#[derive(Debug)]
pub(super) struct Elf<'a> {
    bytes: &'a [u8],
    headers: &'a [[u8; PROGRAM_HEADER]],
    relas: &'a [[u8; RELA]],
    span: Range<u64>,
    alignment: u64,
    instruction: u64,
}

impl<'a> Elf<'a> {
    pub(super) fn read(bytes: &'a [u8], machine: Machine, page_size: u64) -> Result<Self, Error> {
        if bytes.len() > MAX_IMAGE {
            return Err(Error::Capacity);
        }
        if page_size < 4096 || !page_size.is_power_of_two() {
            return Err(Error::Unsupported);
        }
        let header = bytes.first_chunk::<ELF_HEADER>().ok_or(Error::Format)?;
        if &header[..4] != b"\x7fELF"
            || header[6] != 1
            || word(header, 20) != 1
            || half(header, 52) != ELF_HEADER as u16
            || half(header, 54) != PROGRAM_HEADER as u16
        {
            return Err(Error::Format);
        }
        if header[4..6] != [2, 1]
            || !matches!(header[7], 0 | 3)
            || header[8] != 0
            || half(header, 16) != 3
            || half(header, 18) != machine.number()
        {
            return Err(Error::Unsupported);
        }
        let instruction = machine.instruction(word(header, 48))?;
        let offset = usize::try_from(quad(header, 32)).map_err(|_| Error::Range)?;
        let count = usize::from(half(header, 56));
        if offset < ELF_HEADER || !offset.is_multiple_of(8) || count == 0 {
            return Err(Error::Format);
        }
        if count > MAX_HEADERS {
            return Err(Error::Capacity);
        }
        let end = offset
            .checked_add(count * PROGRAM_HEADER)
            .ok_or(Error::Range)?;
        let headers = bytes.get(offset..end).ok_or(Error::Range)?.as_chunks().0;
        let mut elf = Self {
            bytes,
            headers,
            relas: &[],
            span: 0..0,
            alignment: page_size,
            instruction,
        };

        let mut note_bytes = 0;
        let mut dynamic = None;
        let mut phdr_seen = false;
        let mut stack_seen = false;
        for (index, raw) in headers.iter().enumerate() {
            let header = Header::read(raw);
            match header.kind {
                0 => (),
                LOAD => {
                    elf.check_load(header, index, page_size)?;
                }
                DYNAMIC if dynamic.is_none() => {
                    elf.check_mapped(header)?;
                    dynamic = Some(header);
                }
                NOTE => {
                    let range = header.file_range(bytes)?;
                    if header.alignment != 4
                        || range.is_empty()
                        || range.start % 4 != 0
                        || range.len() % 4 != 0
                    {
                        return Err(Error::Format);
                    }
                    note_bytes += header.file_size;
                    if note_bytes > MAX_NOTES {
                        return Err(Error::Capacity);
                    }
                    for prior in headers[..index].iter().map(Header::read) {
                        if prior.kind == NOTE && overlaps(&range, &prior.file_range(bytes)?) {
                            return Err(Error::Conflict);
                        }
                    }
                }
                PHDR if !phdr_seen => {
                    if header.file_range(bytes)? != (offset..end) || header.flags != READ {
                        return Err(Error::Format);
                    }
                    elf.check_mapped(header)?;
                    phdr_seen = true;
                }
                GNU_STACK if !stack_seen => {
                    if header.flags != READ_WRITE {
                        return Err(Error::Unsupported);
                    }
                    if header.file_size != 0 || header.memory_size != 0 {
                        return Err(Error::Format);
                    }
                    stack_seen = true;
                }
                GNU_EH_FRAME if header.flags == READ => elf.check_mapped(header)?,
                RISCV_ATTRIBUTES if machine == Machine::Riscv64 && header.flags == READ => {
                    header.file_range(bytes)?;
                }
                DYNAMIC | PHDR | GNU_STACK => return Err(Error::Conflict),
                _ => return Err(Error::Unsupported),
            }
        }
        if elf.span.is_empty() {
            return Err(Error::Format);
        }
        if elf.span.end - elf.span.start > MAX_IMAGE as u64 {
            return Err(Error::Capacity);
        }
        let entry = quad(header, 24);
        if (entry != 0 && !elf.is_file_executable(entry))
            || !elf.segments().any(|segment| {
                segment.permissions == Permissions::ReadExecute && !segment.file.is_empty()
            })
        {
            return Err(Error::Range);
        }
        if let Some(dynamic) = dynamic {
            elf.relas = elf.read_dynamic(dynamic, machine)?;
        }
        Ok(elf)
    }

    pub(super) fn alignment(&self) -> u64 {
        self.alignment
    }

    pub(super) fn span(&self) -> Range<u64> {
        self.span.clone()
    }

    pub(super) fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        self.headers
            .iter()
            .map(Header::read)
            .filter(|header| header.kind == LOAD)
            .map(|header| Segment {
                address: header.address,
                memory_size: header.memory_size,
                file: self.file(header),
                permissions: header.permissions().expect("checked load permissions"),
            })
    }

    pub(super) fn notes(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.headers
            .iter()
            .map(Header::read)
            .filter(|header| header.kind == NOTE)
            .map(|header| self.file(header))
    }

    pub(super) fn relocations(&self) -> impl ExactSizeIterator<Item = Relocation> + '_ {
        self.relas.iter().map(|bytes| Relocation {
            address: quad(bytes, 0),
            addend: quad(bytes, 16),
        })
    }

    pub(super) fn is_file_executable(&self, address: u64) -> bool {
        address.is_multiple_of(self.instruction)
            && self.segments().any(|segment| {
                if segment.permissions != Permissions::ReadExecute {
                    return false;
                }
                let offset = address
                    .checked_sub(segment.address)
                    .and_then(|offset| usize::try_from(offset).ok());
                let Some(bytes) = offset.and_then(|offset| segment.file.get(offset..)) else {
                    return false;
                };
                if self.instruction != 2 {
                    return bytes.len() >= self.instruction as usize;
                }
                // An RV64 compressed image can still start with a 32-bit
                // instruction. Check its width before accepting the entry.
                matches!(bytes, [first, _, ..] if first & 3 != 3)
                    || matches!(bytes, [first, _, _, _, ..] if first & 3 == 3 && first & 31 != 31)
            })
    }

    fn file(&self, header: Header) -> &'a [u8] {
        &self.bytes[header.file_range(self.bytes).expect("checked file extent")]
    }

    fn check_load(&mut self, header: Header, index: usize, page_size: u64) -> Result<(), Error> {
        header.permissions()?;
        if header.memory_size == 0 || header.file_size > header.memory_size {
            return Err(Error::Range);
        }
        if header.offset % page_size != header.address % page_size
            || (header.alignment > 1
                && (!header.alignment.is_power_of_two()
                    || header.offset % header.alignment != header.address % header.alignment))
        {
            return Err(Error::Format);
        }
        let file = header.file_range(self.bytes)?;
        let pages = header.pages(page_size)?;
        for other in self.headers[..index].iter().map(Header::read) {
            if other.kind == LOAD
                && (overlaps(&pages, &other.pages(page_size)?)
                    || overlaps(&file, &other.file_range(self.bytes)?))
            {
                return Err(Error::Conflict);
            }
            if other.kind == LOAD && header.address < other.address {
                return Err(Error::Format);
            }
        }
        if self.span.is_empty() {
            self.span = pages;
        } else {
            self.span.start = self.span.start.min(pages.start);
            self.span.end = self.span.end.max(pages.end);
        }
        self.alignment = self.alignment.max(header.alignment);
        Ok(())
    }

    // A referenced file range must be wholly backed by one PT_LOAD. File
    // offsets and virtual addresses must describe the same bytes.
    fn file_at(&self, address: u64, length: u64) -> Result<Range<usize>, Error> {
        let end = address.checked_add(length).ok_or(Error::Range)?;
        for header in self
            .headers
            .iter()
            .map(Header::read)
            .filter(|header| header.kind == LOAD)
        {
            if address >= header.address && end <= header.extent(header.file_size)?.end {
                let start = header
                    .offset
                    .checked_add(address - header.address)
                    .ok_or(Error::Range)?;
                return Header {
                    offset: start,
                    file_size: length,
                    ..header
                }
                .file_range(self.bytes);
            }
        }
        Err(Error::Range)
    }

    fn check_mapped(&self, header: Header) -> Result<(), Error> {
        if header.file_size == 0 || header.file_size != header.memory_size {
            return Err(Error::Format);
        }
        if self.file_at(header.address, header.file_size)? != header.file_range(self.bytes)? {
            return Err(Error::Range);
        }
        Ok(())
    }

    fn read_dynamic(&self, header: Header, machine: Machine) -> Result<&'a [[u8; RELA]], Error> {
        if !matches!(header.flags, READ | READ_WRITE) || header.address % 8 != 0 {
            return Err(Error::Format);
        }
        let (entries, remainder) = self.file(header).as_chunks::<DYNAMIC_ENTRY>();
        if !remainder.is_empty() {
            return Err(Error::Format);
        }
        if entries.len() > MAX_DYNAMIC {
            return Err(Error::Capacity);
        }
        let description = self.dynamic_entries(entries)?;
        let relas: &[[u8; RELA]] = match (description.address, description.size, description.entry)
        {
            (None, None, None) if description.count.unwrap_or(0) == 0 => &[],
            (Some(address), Some(size), Some(RELA_SIZE))
                if address % 8 == 0 && size.is_multiple_of(RELA_SIZE) =>
            {
                self.bytes[self.file_at(address, size)?]
                    .as_chunks::<RELA>()
                    .0
            }
            _ => return Err(Error::Format),
        };
        if relas.len() > MAX_RELOCATIONS {
            return Err(Error::Capacity);
        }
        if description
            .count
            .is_some_and(|count| count > relas.len() as u64)
        {
            return Err(Error::Format);
        }
        for (index, rela) in relas.iter().enumerate() {
            self.check_relocation(rela, machine)?;
            let address = quad(rela, 0);
            if relas[..index].iter().any(|prior| quad(prior, 0) == address) {
                return Err(Error::Conflict);
            }
        }
        Ok(relas)
    }

    fn dynamic_entries(&self, entries: &[[u8; DYNAMIC_ENTRY]]) -> Result<Dynamic, Error> {
        let mut description = Dynamic::default();
        for (index, entry) in entries.iter().enumerate() {
            let tag = quad(entry, 0);
            let value = quad(entry, 8);
            if tag == DT_NULL {
                if entries[index..]
                    .iter()
                    .any(|entry| entry.iter().any(|byte| *byte != 0))
                {
                    return Err(Error::Format);
                }
                match (description.strings, description.strings_size) {
                    (Some(address), Some(size)) if size != 0 => {
                        self.file_at(address, size)?;
                    }
                    (None, None) => (),
                    _ => return Err(Error::Format),
                }
                return Ok(description);
            }
            if entries[..index].iter().any(|entry| quad(entry, 0) == tag) {
                return Err(Error::Conflict);
            }
            match tag {
                // Retain these ordinary linker tables as image bytes. The
                // loader consumes neither symbols nor name indexes.
                DT_HASH => {
                    self.file_at(value, 8)?;
                }
                DT_SYMTAB => {
                    self.file_at(value, 24)?;
                }
                DT_GNU_HASH => {
                    self.file_at(value, 16)?;
                }
                DT_STRTAB => description.strings = Some(value),
                DT_STRSZ => description.strings_size = Some(value),
                DT_RELA => description.address = Some(value),
                DT_RELASZ => description.size = Some(value),
                DT_RELAENT => description.entry = Some(value),
                DT_RELACOUNT => description.count = Some(value),
                DT_SYMENT if value == 24 => (),
                DT_SYMBOLIC if value == 0 => (),
                DT_FLAGS if value & !DF_SYMBOLIC == 0 => (),
                _ => return Err(Error::Unsupported),
            }
        }
        Err(Error::Format)
    }

    fn check_relocation(&self, bytes: &[u8; RELA], machine: Machine) -> Result<(), Error> {
        // The upper word is the symbol index. The permitted relative
        // relocation must name symbol zero and needs no resolver.
        if quad(bytes, 8) != machine.relative() {
            return Err(Error::Unsupported);
        }
        let address = quad(bytes, 0);
        let end = address.checked_add(8).ok_or(Error::Range)?;
        let addend = quad(bytes, 16);
        if !address.is_multiple_of(8) || addend > i64::MAX as u64 {
            return Err(Error::Range);
        }
        if !self.segments().any(|segment| {
            segment.permissions == Permissions::ReadWrite
                && address >= segment.address
                && end <= segment.address + segment.memory_size
        }) || !self.segments().any(|segment| {
            (segment.address..segment.address + segment.memory_size).contains(&addend)
        }) {
            return Err(Error::Range);
        }
        Ok(())
    }
}

const RELA_SIZE: u64 = RELA as u64;

#[derive(Default)]
struct Dynamic {
    address: Option<u64>,
    size: Option<u64>,
    entry: Option<u64>,
    count: Option<u64>,
    strings: Option<u64>,
    strings_size: Option<u64>,
}

fn overlaps<T: Ord>(first: &Range<T>, second: &Range<T>) -> bool {
    !first.is_empty() && !second.is_empty() && first.start < second.end && second.start < first.end
}

fn half(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn quad(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
