//! Shared admission of the major CPU and launch of minor CPUs.
pub(crate) mod major;
pub(crate) mod minor;
pub(crate) mod trap;

/// Major and minor CPUs use the same ordinary startup stack budget.
pub(in crate::metal) const STACK_BYTES: usize = 32 * 1024;
