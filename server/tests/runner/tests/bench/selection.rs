use super::{Candidate, CargoImage, Selection, candidates};
use serde_json::json;
use std::path::PathBuf;

fn artifact(kind: &str, target: &str, image: &str) -> String {
    json!({
        "reason": "compiler-artifact",
        "package_id": "fixture",
        "manifest_path": "/compiled/Cargo.toml",
        "target": {"kind": [kind], "name": target},
        "profile": {"test": true},
        "executable": image,
    })
    .to_string()
}

fn candidate(kind: &str, purpose: &str, target: &str) -> Candidate {
    Candidate {
        cargo: CargoImage {
            package: "fixture".into(),
            target: target.into(),
            kind: kind.into(),
            manifest: PathBuf::from("/compiled/Cargo.toml"),
            image: PathBuf::from(format!("/compiled/{target}")),
        },
        image_sha256: "a".repeat(64),
        architecture: "riscv64".into(),
        purpose: purpose.into(),
    }
}

#[test]
fn cargo_discovery_requires_a_complete_build_and_actual_executables() {
    let test = artifact("test", "arbitrary", "/compiled/arbitrary");
    let bench = artifact("bench", "other", "/compiled/other");
    let complete = json!({"reason": "build-finished", "success": true});
    let failed = json!({"reason": "build-finished", "success": false});
    let images = candidates(&format!("{test}\n{bench}\n{complete}\n")).unwrap();
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].kind, "test");
    assert_eq!(images[1].kind, "bench");
    assert!(candidates(&test).is_err());
    assert!(candidates(&format!("{test}\n{failed}")).is_err());
    assert!(candidates(&format!("{complete}\n{test}")).is_err());
    assert!(candidates(&complete.to_string()).is_err());
}

#[test]
fn only_measurement_purpose_is_selected() {
    let mut selection = Selection {
        schema: 1,
        candidates: vec![
            candidate("test", "test", "ordinary"),
            candidate("test", "bench", "shared"),
            candidate("bench", "bench", "dedicated"),
        ],
    };
    let selected = selection.selected().unwrap();
    assert_eq!(selected.len(), 2);
    assert!(selected.contains_key("shared"));
    assert!(selected.contains_key("dedicated"));
    assert!(!selected.contains_key("ordinary"));

    selection.candidates.push(candidate("bench", "test", "invalid"));
    assert!(selection.selected().is_err());
    selection.candidates.pop();
    selection.candidates.push(candidate("test", "bench", "shared"));
    assert!(selection.selected().is_err());
    selection.candidates = vec![candidate("test", "test", "ordinary")];
    assert!(selection.selected().is_err());
}
