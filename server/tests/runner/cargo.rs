//! Decode the complete native harness artifact stream produced by Cargo.
use crate::{Result, require};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Test,
    Bench,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Image {
    pub package: String,
    pub target: String,
    pub kind: Kind,
    pub manifest: PathBuf,
    pub image: PathBuf,
}

fn string<'a>(record: &'a Value, field: &str) -> Result<&'a str> {
    record[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing Cargo artifact field: {field}").into())
}

pub fn images(log: &str) -> Result<Vec<Image>> {
    let mut built = false;
    let mut images = Vec::new();
    for line in log.lines() {
        let record: Value = serde_json::from_str(line)?;
        require(!built, "Cargo message after build completion")?;
        match record["reason"].as_str() {
            Some("compiler-artifact") => {
                let target = &record["target"];
                let kind = match target["kind"].as_array().map(Vec::as_slice) {
                    Some([kind]) if kind == "test" => Kind::Test,
                    Some([kind]) if kind == "bench" => Kind::Bench,
                    _ => continue,
                };
                require(
                    record["profile"]["test"] == true,
                    "expected a Cargo harness artifact",
                )?;
                images.push(Image {
                    package: string(&record, "package_id")?.to_owned(),
                    target: string(target, "name")?.to_owned(),
                    kind,
                    manifest: PathBuf::from(string(&record, "manifest_path")?),
                    image: PathBuf::from(string(&record, "executable")?),
                });
            }
            Some("build-finished") => {
                require(record["success"] == true, "Cargo build did not succeed")?;
                built = true;
            }
            Some(_) => {}
            None => return Err("missing Cargo message reason".into()),
        }
    }
    require(
        built && !images.is_empty(),
        "incomplete Cargo artifact stream",
    )?;
    Ok(images)
}

#[cfg(test)]
#[path = "tests/cargo.rs"]
mod tests;
