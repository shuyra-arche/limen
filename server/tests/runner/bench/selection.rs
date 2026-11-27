//! Select the benchmark purpose from actual Cargo artifacts, not target names.
use super::run_in;
use crate::{Result, evidence, platform, require};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
struct CargoImage {
    package: String,
    target: String,
    kind: String,
    manifest: PathBuf,
    image: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct Candidate {
    #[serde(flatten)]
    cargo: CargoImage,
    image_sha256: String,
    architecture: String,
    purpose: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Selection {
    schema: u32,
    candidates: Vec<Candidate>,
}

fn string<'a>(record: &'a Value, field: &str) -> Result<&'a str> {
    record[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing Cargo artifact field: {field}").into())
}

fn name(image: &Path) -> Result<String> {
    Ok(image
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("missing image file name")?
        .to_owned())
}

fn candidates(log: &str) -> Result<Vec<CargoImage>> {
    let mut built = false;
    let mut images = Vec::new();
    for line in log.lines() {
        let record: Value = serde_json::from_str(line)?;
        require(!built, "Cargo message after build completion")?;
        match record["reason"].as_str() {
            Some("compiler-artifact") => {
                let target = &record["target"];
                let kind = match target["kind"].as_array().map(Vec::as_slice) {
                    Some([kind]) if kind == "test" => "test",
                    Some([kind]) if kind == "bench" => "bench",
                    _ => continue,
                };
                require(
                    record["profile"]["test"] == true,
                    "expected a Cargo harness artifact",
                )?;
                images.push(CargoImage {
                    package: string(&record, "package_id")?.to_owned(),
                    target: string(target, "name")?.to_owned(),
                    kind: kind.to_owned(),
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
    require(built && !images.is_empty(), "incomplete Cargo artifact stream")?;
    Ok(images)
}

impl Selection {
    fn collect(log: &str) -> Result<Self> {
        let mut result = Self {
            schema: 1,
            candidates: Vec::new(),
        };
        for mut cargo in candidates(log)? {
            cargo.image = cargo.image.canonicalize()?;
            cargo.manifest = cargo.manifest.canonicalize()?;
            require(cargo.manifest.is_file(), "missing Cargo manifest")?;
            let (machine, purpose) = platform::inspect(&cargo.image)?;
            result.candidates.push(Candidate {
                image_sha256: evidence::digest(&cargo.image)?,
                cargo,
                architecture: machine.architecture.into(),
                purpose: purpose.name().into(),
            });
        }
        result.selected()?;
        Ok(result)
    }

    fn selected(&self) -> Result<BTreeMap<String, String>> {
        require(self.schema == 1, "unsupported benchmark selection")?;
        let mut names = BTreeSet::new();
        let mut targets = BTreeSet::new();
        let mut selected = BTreeMap::new();
        for candidate in &self.candidates {
            let name = name(&candidate.cargo.image)?;
            require(names.insert(name.clone()), "duplicate image evidence name")?;
            require(
                !candidate.cargo.package.is_empty()
                    && !candidate.cargo.target.is_empty()
                    && targets.insert((
                        &candidate.cargo.package,
                        &candidate.cargo.kind,
                        &candidate.cargo.target,
                    )),
                "missing or duplicate Cargo target identity",
            )?;
            require(
                candidate.image_sha256.len() == 64
                    && candidate
                        .image_sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit()),
                "invalid selected image digest",
            )?;
            require(
                matches!(candidate.architecture.as_str(), "riscv64" | "aarch64" | "x86_64"),
                "invalid selected architecture",
            )?;
            match (candidate.cargo.kind.as_str(), candidate.purpose.as_str()) {
                ("test" | "bench", "bench") => {
                    selected.insert(name, candidate.image_sha256.clone());
                }
                ("test", "test") => {}
                _ => return Err("invalid artifact kind or purpose".into()),
            }
        }
        require(!selected.is_empty(), "no benchmark-purpose images")?;
        Ok(selected)
    }
}

/// Historical artifact validation needs the exact selected image set.
pub(super) fn expected(path: &Path) -> Result<BTreeMap<String, String>> {
    let selection: Selection = serde_json::from_slice(&fs::read(path)?)?;
    selection.selected()
}

pub fn run_set(artifacts: &Path, root: &Path) -> Result<()> {
    let selection = Selection::collect(&fs::read_to_string(artifacts)?)?;
    let root = root.canonicalize()?;
    let directory = root.join("bench");
    evidence::fresh(&directory)?;
    let selection_path = root.join("selection.json");
    require(!selection_path.exists(), "selection evidence already exists")?;
    evidence::json(&selection_path, &selection)?;

    let mut failures = Vec::new();
    for candidate in &selection.candidates {
        if candidate.purpose != "bench" {
            println!("Not selected (test purpose): {}", candidate.cargo.target);
            continue;
        }
        println!("Measure: {}", candidate.cargo.target);
        let result = (|| {
            require(
                evidence::digest(&candidate.cargo.image)? == candidate.image_sha256,
                "compiled image changed after selection",
            )?;
            let working = candidate
                .cargo
                .manifest
                .parent()
                .ok_or("missing Package root")?;
            run_in(&candidate.cargo.image, &directory, working)
        })();
        if let Err(error) = result {
            eprintln!("{}: {error}", candidate.cargo.target);
            failures.push(candidate.cargo.target.as_str());
        }
    }
    require(
        failures.is_empty(),
        format!("benchmark images failed: {}", failures.join(", ")),
    )
}

#[cfg(test)]
#[path = "../tests/bench/selection.rs"]
mod tests;
