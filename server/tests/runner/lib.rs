//! Host-only execution and evidence for native guest tests.
pub mod native;
pub mod platform;
pub mod process;
pub mod qmp;
pub mod utilities;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, process::Command};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
pub fn json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}
pub fn digest(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn output(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    require(
        result.status.success(),
        format!("{command:?}: {}", String::from_utf8_lossy(&result.stderr)),
    )?;
    Ok(String::from_utf8(result.stdout)?.trim().into())
}
pub fn fresh(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(path)?;
    Ok(())
}
