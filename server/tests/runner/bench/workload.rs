//! Local source identity from the compiled Cargo image's Rust dep-info.
use crate::{Result, evidence, require};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Workload {
    schema: u32,
    inputs: BTreeMap<String, String>,
}

fn dependencies(text: &str) -> Result<Vec<PathBuf>> {
    // rustc's first makefile rule names the actual crate inputs. The
    // following rules repeat inputs as empty targets, not dependencies.
    let text = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let rule = text.lines().next().ok_or("missing dependency rule")?;
    let (_, inputs) = rule.split_once(": ").ok_or("invalid dependency rule")?;
    let mut paths = Vec::new();
    let mut path = String::new();
    let mut escaped = false;
    for character in inputs.chars() {
        if escaped {
            require(
                character.is_ascii_whitespace() || character == '\\',
                "unsupported dependency escape",
            )?;
            path.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character.is_ascii_whitespace() {
            if !path.is_empty() {
                paths.push(PathBuf::from(core::mem::take(&mut path)));
            }
        } else {
            path.push(character);
        }
    }
    require(!escaped, "truncated dependency path")?;
    if !path.is_empty() {
        paths.push(path.into());
    }
    require(!paths.is_empty(), "missing compiled inputs")?;
    Ok(paths)
}

fn resolve(root: &Path, working: &Path, path: &Path) -> Result<PathBuf> {
    let mut matches = BTreeSet::new();
    for base in [
        root.to_path_buf(),
        root.join("server/tests/target"),
        working.to_path_buf(),
    ] {
        if let Ok(candidate) = base.join(path).canonicalize()
            && candidate.is_file()
            && candidate.starts_with(root)
        {
            matches.insert(candidate);
        }
    }
    require(
        matches.len() == 1,
        format!("unresolved or ambiguous dependency: {}", path.display()),
    )?;
    Ok(matches.into_iter().next().unwrap())
}

impl Workload {
    fn collect(root: &Path, working: &Path, dep_info: &str) -> Result<Self> {
        let mut inputs = BTreeMap::new();
        for path in dependencies(dep_info)? {
            let file = resolve(root, working, &path)?;
            let relative = file.strip_prefix(root)?;
            let name = relative.to_str().ok_or("non-UTF-8 workload path")?;
            inputs.insert(name.to_owned(), evidence::digest(&file)?);
        }
        let workload = Self { schema: 1, inputs };
        workload.validate()?;
        Ok(workload)
    }

    fn validate(&self) -> Result<()> {
        require(self.schema == 1, "unsupported workload identity")?;
        let mut owner = false;
        let mut entry = false;
        for (name, digest) in &self.inputs {
            let path = Path::new(name);
            require(
                path.components()
                    .all(|part| matches!(part, Component::Normal(_))),
                "invalid workload path",
            )?;
            require(
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid workload digest",
            )?;
            owner |= path.starts_with("server/benches");
            entry |= path.starts_with("server/tests/target")
                && path
                    .strip_prefix("server/tests/target")?
                    .components()
                    .any(|part| matches!(part.as_os_str().to_str(), Some("tests" | "benches")));
        }
        require(owner && entry, "missing benchmark entry or owner inputs")
    }

    pub(super) fn read(path: &Path) -> Result<Self> {
        let workload: Self = serde_json::from_slice(&fs::read(path)?)?;
        workload.validate()?;
        Ok(workload)
    }
}

pub(super) fn capture(image: &Path, working: &Path) -> Result<Workload> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let dep_info = image.canonicalize()?.with_extension("d");
    let contents = fs::read_to_string(&dep_info)?;
    Workload::collect(&root, working, &contents)
}

#[cfg(test)]
#[path = "../tests/bench/workload.rs"]
mod tests;
