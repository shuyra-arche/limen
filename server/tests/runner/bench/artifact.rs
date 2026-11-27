//! Read and validate a completed artifact independently of comparison.
use super::{
    measurement::{Series, decode},
    statistics, workload::Workload,
};
use crate::{Result, require};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

pub(super) struct Case {
    pub(super) image_sha256: String,
    pub(super) machine: Value,
    pub(super) workload: Workload,
    pub(super) series: Series,
}

pub(super) struct Artifact {
    pub(super) source: String,
    pub(super) context: Value,
    pub(super) cases: BTreeMap<String, Case>,
}

fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("missing or empty {name}").into())
}

fn hash(value: &str, length: usize) -> Result<()> {
    require(
        value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid recorded digest",
    )
}

impl Artifact {
    pub(super) fn read(root: &Path) -> Result<Self> {
        let outcomes = read(&root.join("stages/outcomes.json"))?;
        for stage in [
            "base_tools",
            "tools",
            "toolchain",
            "source",
            "runner",
            "context",
            "bench",
        ] {
            require(
                outcomes[stage] == "success",
                format!("incomplete stage: {stage}"),
            )?;
        }
        let source = fs::read_to_string(root.join("source.txt"))?
            .trim()
            .to_owned();
        hash(&source, 40)?;
        let context = read(&root.join("benchmark-context.json"))?;
        require(context["schema"] == 2, "unsupported benchmark context")?;
        for name in [
            "target",
            "profile",
            "rustc",
            "cargo",
            "host_kernel",
            "host_os",
            "host_cpu",
            "host_cpus",
            "host_cpu_quota",
        ] {
            field(&context, name)?;
        }
        require(context["profile"] == "bench", "not an optimized benchmark")?;
        hash(field(&context, "execution_sha256")?, 64)?;
        hash(field(&context, "qemu_sha256")?, 64)?;
        for tool in ["rustc", "cargo"] {
            let recorded = fs::read_to_string(root.join(format!("{tool}.txt")))?;
            require(
                recorded.trim_end() == field(&context, tool)?,
                "conflicting compiler or Cargo identity",
            )?;
        }

        let mut cases = BTreeMap::new();
        for entry in fs::read_dir(root.join("bench"))? {
            let entry = entry?;
            require(
                entry.file_type()?.is_dir(),
                "unexpected benchmark artifact entry",
            )?;
            let directory = entry.path();
            let result = read(&directory.join("result.json"))?;
            let process = read(&directory.join("process.json"))?;
            let cleanup = &process["cleanup"];
            require(
                result["verdict"] == "pass"
                    && process["termination"]["kind"] == "exited"
                    && process["termination"]["value"] == 97
                    && process.get("error").is_some_and(Value::is_null)
                    && cleanup["spawned"] == true
                    && cleanup["settled"] == true
                    && cleanup["unexpected_descendants"] == false
                    && cleanup.get("error").is_some_and(Value::is_null),
                "incomplete execution or retirement evidence",
            )?;

            let workload = Workload::read(&directory.join("workload.json"))?;
            let log = fs::read_to_string(directory.join("serial.log"))?;
            let measurement = decode(&log)?;
            require(
                read(&directory.join("measurement.json"))? == serde_json::to_value(&measurement)?,
                "measurement disagrees with recorded samples",
            )?;
            require(
                read(&directory.join("summary.json"))?
                    == serde_json::to_value(statistics::summarize(&measurement))?,
                "summary disagrees with recorded samples",
            )?;

            let mut platform = read(&directory.join("platform.json"))?;
            let image_sha256 = field(&platform, "image_sha256")?.to_owned();
            hash(&image_sha256, 64)?;
            platform.as_object_mut().unwrap().remove("image_sha256");
            let harts = platform["harts"].as_u64().ok_or("missing Hart count")?;
            require(harts != 0, "invalid Hart count")?;
            let mut firmware = read(&directory.join("firmware.json"))?;
            hash(field(&firmware, "code_sha256")?, 64)?;
            if let Some(variables) = firmware.get("initial_vars_sha256") {
                hash(
                    variables
                        .as_str()
                        .ok_or("invalid firmware variables digest")?,
                    64,
                )?;
            }
            firmware.as_object_mut().unwrap().remove("code");
            let tools = read(&directory.join("tools.json"))?;
            hash(field(&tools, "runner_sha256")?, 64)?;
            let machine = json!({
                "platform": platform,
                "firmware": firmware,
                "qemu": field(&tools, "qemu")?,
                "environment": measurement.environment,
                "hart": measurement.hart,
                "frequency_hz": measurement.frequency_hz,
            });

            for series in measurement.series {
                require(
                    series.participants as u64 <= harts,
                    "series exceeds admitted Hart count",
                )?;
                let name = series.name.clone();
                let case = Case {
                    image_sha256: image_sha256.clone(),
                    machine: machine.clone(),
                    workload: workload.clone(),
                    series,
                };
                require(
                    cases.insert(name, case).is_none(),
                    "duplicate benchmark series across images",
                )?;
            }
        }
        require(!cases.is_empty(), "no complete benchmark images")?;
        Ok(Self {
            source,
            context,
            cases,
        })
    }
}
