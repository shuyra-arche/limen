use super::{Kind, images};
use serde_json::json;

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

#[test]
fn cargo_discovery_requires_a_complete_build_and_actual_executables() {
    let test = artifact("test", "arbitrary", "/compiled/arbitrary");
    let bench = artifact("bench", "other", "/compiled/other");
    let complete = json!({"reason": "build-finished", "success": true});
    let failed = json!({"reason": "build-finished", "success": false});
    let found = images(&format!("{test}\n{bench}\n{complete}\n")).unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].kind, Kind::Test);
    assert_eq!(found[1].kind, Kind::Bench);
    assert!(images(&test).is_err());
    assert!(images(&format!("{test}\n{failed}")).is_err());
    assert!(images(&format!("{complete}\n{test}")).is_err());
    assert!(images(&complete.to_string()).is_err());
}
