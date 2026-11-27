//! Host-only execution and evidence for native guest tests.
pub mod bench;
pub mod cargo;
pub mod evidence;
pub mod machine;
pub mod native;
pub mod platform;
pub mod process;
pub mod qmp;
pub mod utilities;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
