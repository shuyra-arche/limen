//! Described RISC-V execution extensions and their exact Hart scope.

use crate::metal::{
    firmware::devicetree::{Error, NodeId},
    hart::description::RiscvCpu,
};

const EXTENSION_LIMIT: usize = 192;
const NAME_LIMIT: usize = 63;
const ISA_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum RiscvExtension {
    Integer = 1 << 0,
    MultiplyDivide = 1 << 1,
    Atomic = 1 << 2,
    Float = 1 << 3,
    Double = 1 << 4,
    Compressed = 1 << 5,
    Vector = 1 << 6,
    Csr = 1 << 7,
    InstructionFence = 1 << 8,
    SupervisorTimer = 1 << 9,
}

impl RiscvExtension {
    fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"i" => Self::Integer,
            b"m" => Self::MultiplyDivide,
            b"a" => Self::Atomic,
            b"f" => Self::Float,
            b"d" => Self::Double,
            b"c" => Self::Compressed,
            b"v" => Self::Vector,
            b"zicsr" => Self::Csr,
            b"zifencei" => Self::InstructionFence,
            b"sstc" => Self::SupervisorTimer,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
pub struct RiscvIsa<'a> {
    source: NodeId<'a>,
    extensions: u16,
}

impl<'a> RiscvIsa<'a> {
    /// Decode the RV64I base and execution extensions from a described Hart.
    /// Unknown extension names stay available in the original CPU node.
    pub fn describe(cpu: &RiscvCpu<'_, 'a>) -> Result<Option<Self>, Error> {
        let cpu = cpu.node();
        let base = cpu.string(b"riscv,isa-base")?;
        let list = cpu.property(b"riscv,isa-extensions")?;
        let modern = match (base, list) {
            (None, None) => None,
            (Some(base), Some(list)) => Some(modern_extensions(base, list)?),
            _ => return Err(Error::Missing),
        };
        let legacy = cpu
            .string(b"riscv,isa")?
            .map(legacy_extensions)
            .transpose()?;
        if let (Some(modern), Some(legacy)) = (modern, legacy) {
            // The compact letters describe these same execution features in
            // both formats. Legacy Zicsr/Zifencei can be implicit in the base.
            const COMPARE: u16 = (RiscvExtension::Vector as u16 * 2) - 1;
            if modern & COMPARE != legacy & COMPARE {
                return Err(Error::Conflict);
            }
        }
        Ok(modern.or(legacy).map(|extensions| Self {
            source: cpu.id(),
            extensions,
        }))
    }

    pub fn source(self) -> NodeId<'a> {
        self.source
    }

    pub fn has(self, extension: RiscvExtension) -> bool {
        self.extensions & extension as u16 != 0
    }
}

fn modern_extensions(base: &[u8], list: &[u8]) -> Result<u16, Error> {
    if base != b"rv64i" {
        return Err(Error::Unsupported);
    }
    if list.len() > ISA_LIMIT {
        return Err(Error::Capacity);
    }
    let list = list.strip_suffix(b"\0").ok_or(Error::Format)?;
    let names = list.split(|byte| *byte == 0);
    let mut extensions = RiscvExtension::Integer as u16;
    for (index, name) in names.clone().enumerate() {
        if index == EXTENSION_LIMIT {
            return Err(Error::Capacity);
        }
        validate_name(name)?;
        if names.clone().take(index).any(|previous| previous == name) {
            return Err(Error::Conflict);
        }
        extensions |= RiscvExtension::from_name(name).map_or(0, |extension| extension as u16);
    }
    Ok(extensions)
}

fn legacy_extensions(isa: &[u8]) -> Result<u16, Error> {
    if isa.len() > ISA_LIMIT {
        return Err(Error::Capacity);
    }
    let suffix = isa.strip_prefix(b"rv64").ok_or(Error::Unsupported)?;
    if !matches!(suffix.first(), Some(b'i' | b'g')) {
        return Err(Error::Unsupported);
    }
    let compact_end = suffix
        .iter()
        .position(|byte| matches!(byte, b'_' | b's' | b'x' | b'z'))
        .unwrap_or(suffix.len());
    let (mut compact, named) = suffix.split_at(compact_end);
    let mut letters = 0u32;
    // The legacy binding includes CSR and instruction-fence operations in I.
    let mut extensions = RiscvExtension::Csr as u16 | RiscvExtension::InstructionFence as u16;
    while let Some((&letter, rest)) = compact.split_first() {
        if !letter.is_ascii_lowercase() {
            return Err(Error::Format);
        }
        let bit = 1u32 << (letter - b'a');
        if letters & bit != 0 {
            return Err(Error::Conflict);
        }
        letters |= bit;
        extensions |= if letter == b'g' {
            RiscvExtension::Integer as u16
                | RiscvExtension::MultiplyDivide as u16
                | RiscvExtension::Atomic as u16
                | RiscvExtension::Float as u16
                | RiscvExtension::Double as u16
        } else {
            RiscvExtension::from_name(&[letter]).map_or(0, |extension| extension as u16)
        };
        compact = after_version(rest);
    }
    if named.is_empty() {
        return Ok(extensions);
    }
    let names = named
        .strip_prefix(b"_")
        .unwrap_or(named)
        .split(|byte| *byte == b'_');
    for (index, name) in names.clone().enumerate() {
        if index == EXTENSION_LIMIT {
            return Err(Error::Capacity);
        }
        validate_name(name)?;
        if names.clone().take(index).any(|previous| previous == name) {
            return Err(Error::Conflict);
        }
        if let Some(extension) = [
            (b"zicsr".as_slice(), RiscvExtension::Csr),
            (b"zifencei", RiscvExtension::InstructionFence),
            (b"sstc", RiscvExtension::SupervisorTimer),
        ]
        .into_iter()
        .find_map(|(candidate, extension)| {
            name.strip_prefix(candidate)
                .filter(|suffix| after_version(suffix).is_empty())
                .map(|_| extension)
        }) {
            extensions |= extension as u16;
        }
    }
    Ok(extensions)
}

fn validate_name(name: &[u8]) -> Result<(), Error> {
    if name.len() > NAME_LIMIT {
        return Err(Error::Capacity);
    }
    if !name.first().is_some_and(u8::is_ascii_lowercase)
        || !name
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Err(Error::Format);
    }
    Ok(())
}

fn after_version(value: &[u8]) -> &[u8] {
    let digits = value
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let rest = &value[digits..];
    if digits > 0
        && let [b'p', minor, ..] = rest
        && minor.is_ascii_digit()
    {
        let minor = rest[1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        return &rest[1 + minor..];
    }
    rest
}
