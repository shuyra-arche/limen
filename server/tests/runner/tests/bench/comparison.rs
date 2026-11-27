use super::{Artifact, report};
use crate::{bench::measurement::decode, evidence};
use serde_json::json;
use std::{fs, path::Path};

fn artifact(root: &Path, source: char, sample: u64) {
    let image = root.join("bench/image");
    fs::create_dir_all(&image).unwrap();
    fs::create_dir_all(root.join("stages")).unwrap();
    let mut outcomes = serde_json::Map::new();
    for stage in [
        "base_tools",
        "tools",
        "toolchain",
        "source",
        "runner",
        "context",
        "build",
        "bench",
    ] {
        outcomes.insert(stage.into(), json!("success"));
    }
    evidence::json(&root.join("stages/outcomes.json"), &outcomes).unwrap();
    fs::write(root.join("source.txt"), source.to_string().repeat(40)).unwrap();
    fs::write(root.join("rustc.txt"), "rustc fixture\n").unwrap();
    fs::write(root.join("cargo.txt"), "cargo fixture\n").unwrap();
    let digest = "a".repeat(64);
    evidence::json(
        &root.join("selection.json"),
        &json!({"schema": 1, "candidates": [{
            "package": "fixture", "target": "fixture", "kind": "bench",
            "manifest": "/compiled/Cargo.toml", "image": "/compiled/image",
            "image_sha256": source.to_string().repeat(64),
            "architecture": "riscv64", "purpose": "bench",
        }]}),
    )
    .unwrap();
    evidence::json(
        &root.join("benchmark-context.json"),
        &json!({
            "schema": 2, "target": "riscv64gc-unknown-none-elf", "profile": "bench",
            "rustc": "rustc fixture", "cargo": "cargo fixture",
            "host_kernel": "fixture", "host_os": "fixture", "host_cpu": "fixture",
            "host_cpus": "4", "host_cpu_quota": "max 100000",
            "execution_sha256": digest, "qemu_sha256": digest,
        }),
    )
    .unwrap();
    evidence::json(
        &image.join("workload.json"),
        &json!({"schema": 1, "inputs": {
            "server/benches/metal/chrono/clock.rs": digest,
            "server/tests/target/riscv64/benches/clock.rs": digest,
        }}),
    )
    .unwrap();
    evidence::json(&image.join("result.json"), &json!({"verdict": "pass"})).unwrap();
    evidence::json(
        &image.join("process.json"),
        &json!({
            "termination": {"kind": "exited", "value": 97}, "error": null,
            "cleanup": {"spawned": true, "settled": true,
                "unexpected_descendants": false, "error": null},
        }),
    )
    .unwrap();
    evidence::json(
        &image.join("platform.json"),
        &json!({"architecture": "riscv64", "purpose": "bench", "harts": 1,
            "image_sha256": source.to_string().repeat(64)}),
    )
    .unwrap();
    evidence::json(
        &image.join("firmware.json"),
        &json!({"code": "fixture", "code_sha256": digest}),
    )
    .unwrap();
    evidence::json(
        &image.join("tools.json"),
        &json!({"qemu": "fixture", "runner_sha256": digest}),
    )
    .unwrap();
    let log = format!(
        "LIMEN_BENCH_V2 0 10000000 1\nLIMEN_BENCH_SERIES fixture 1 1 10 1\nLIMEN_BENCH_SAMPLE 0 {sample}\n"
    );
    fs::write(image.join("serial.log"), &log).unwrap();
    let measurement = decode(&log).unwrap();
    evidence::json(&image.join("measurement.json"), &measurement).unwrap();
    evidence::json(
        &image.join("summary.json"),
        &crate::bench::statistics::summarize(&measurement),
    )
    .unwrap();
}

#[test]
fn comparison_rejects_changed_context_and_incomplete_evidence() {
    let root = std::env::temp_dir().join(format!("kernel-compare-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let before = root.join("before");
    let after = root.join("after");
    artifact(&before, 'a', 100);
    artifact(&after, 'b', 150);
    let baseline = Artifact::read(&before).unwrap();
    let mut candidate = Artifact::read(&after).unwrap();
    let result = report(&baseline, &candidate).unwrap();
    assert_eq!(
        result["series"][0]["median_duration_ratio"].as_f64(),
        Some(1.5)
    );

    candidate.context["host_cpu"] = json!("different");
    assert!(report(&baseline, &candidate).is_err());

    let mut candidate = Artifact::read(&after).unwrap();
    let workload_path = after.join("bench/image/workload.json");
    let mut workload: serde_json::Value =
        serde_json::from_slice(&fs::read(&workload_path).unwrap()).unwrap();
    workload["inputs"]["server/benches/metal/chrono/clock.rs"] = json!("b".repeat(64));
    evidence::json(&workload_path, &workload).unwrap();
    candidate.cases.get_mut("fixture").unwrap().workload =
        crate::bench::workload::Workload::read(&workload_path).unwrap();
    let changed = report(&baseline, &candidate).unwrap();
    assert_eq!(changed["series"], json!([]));
    assert_eq!(
        changed["uncompared"][0]["reason"],
        "different workload inputs"
    );

    fs::remove_file(after.join("bench/image/measurement.json")).unwrap();
    assert!(Artifact::read(&after).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unrelated_series_remains_comparable_when_another_workload_changes() {
    let root = std::env::temp_dir().join(format!("kernel-compare-local-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let before = root.join("before");
    let after = root.join("after");
    artifact(&before, 'a', 100);
    artifact(&after, 'b', 150);
    let mut baseline = Artifact::read(&before).unwrap();
    let mut candidate = Artifact::read(&after).unwrap();

    for (artifact, directory) in [(&mut baseline, &before), (&mut candidate, &after)] {
        let mut independent = Artifact::read(directory).unwrap();
        let mut case = independent.cases.remove("fixture").unwrap();
        case.series.name = "unchanged".into();
        artifact.cases.insert("unchanged".into(), case);
    }
    let workload_path = after.join("bench/image/workload.json");
    let mut workload: serde_json::Value =
        serde_json::from_slice(&fs::read(&workload_path).unwrap()).unwrap();
    workload["inputs"]["server/benches/metal/chrono/clock.rs"] = json!("b".repeat(64));
    evidence::json(&workload_path, &workload).unwrap();
    candidate.cases.get_mut("fixture").unwrap().workload =
        crate::bench::workload::Workload::read(&workload_path).unwrap();

    let result = report(&baseline, &candidate).unwrap();
    assert_eq!(result["series"].as_array().unwrap().len(), 1);
    assert_eq!(result["series"][0]["name"], "unchanged");
    assert_eq!(result["uncompared"][0]["name"], "fixture");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn artifact_requires_every_selected_image_and_no_unselected_evidence() {
    let root = std::env::temp_dir().join(format!("kernel-selection-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    artifact(&root, 'a', 100);
    assert!(Artifact::read(&root).is_ok());

    let path = root.join("selection.json");
    let mut selection: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    selection["candidates"].as_array_mut().unwrap().push(json!({
        "package": "fixture", "target": "missing", "kind": "test",
        "manifest": "/compiled/Cargo.toml", "image": "/compiled/missing",
        "image_sha256": "b".repeat(64),
        "architecture": "riscv64", "purpose": "bench",
    }));
    evidence::json(&path, &selection).unwrap();
    assert!(Artifact::read(&root).is_err());

    selection["candidates"][1]["purpose"] = json!("test");
    evidence::json(&path, &selection).unwrap();
    assert!(Artifact::read(&root).is_ok());

    selection["candidates"][0]["image_sha256"] = json!("c".repeat(64));
    evidence::json(&path, &selection).unwrap();
    assert!(Artifact::read(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}
