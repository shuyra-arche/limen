//! Compare only measurements with compatible recorded conditions.
use super::{artifact::Artifact, statistics};
use crate::{Result, require};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

fn report(baseline: &Artifact, candidate: &Artifact) -> Result<Value> {
    require(
        baseline.context == candidate.context,
        "different recorded execution contexts; comparison refused",
    )?;
    let names: BTreeSet<_> = baseline
        .cases
        .keys()
        .chain(candidate.cases.keys())
        .collect();
    let mut series = Vec::new();
    let mut uncompared = Vec::new();
    for name in names {
        let (Some(before), Some(after)) = (baseline.cases.get(name), candidate.cases.get(name))
        else {
            uncompared.push(json!({"name": name, "reason": "missing matching series"}));
            continue;
        };
        if before.workload != after.workload {
            uncompared.push(json!({"name": name, "reason": "different workload inputs"}));
            continue;
        }
        let a = &before.series;
        let b = &after.series;
        if before.machine != after.machine
            || a.participants != b.participants
            || a.warmup_iterations != b.warmup_iterations
            || a.iterations != b.iterations
            || a.sample_count != b.sample_count
        {
            uncompared.push(json!({
                "name": name,
                "reason": "different machine or series definition",
            }));
            continue;
        }
        let prior = statistics::series(a);
        let current = statistics::series(b);
        series.push(json!({
            "name": name,
            "baseline_image_sha256": before.image_sha256,
            "candidate_image_sha256": after.image_sha256,
            "baseline": prior,
            "candidate": current,
            "median_duration_ratio": current.median_batch_ns / prior.median_batch_ns,
        }));
    }
    Ok(json!({
        "baseline_source": baseline.source,
        "candidate_source": candidate.source,
        "interpretation": "descriptive QEMU measurements; no regression threshold",
        "series": series,
        "uncompared": uncompared,
    }))
}

pub fn compare(baseline: &Path, candidate: &Path) -> Result<()> {
    let baseline = Artifact::read(baseline)?;
    let candidate = Artifact::read(candidate)?;
    let report = report(&baseline, &candidate)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    require(
        report["series"]
            .as_array()
            .is_some_and(|series| !series.is_empty()),
        "no comparable benchmark series",
    )
}

#[cfg(test)]
#[path = "../tests/bench/comparison.rs"]
mod tests;
