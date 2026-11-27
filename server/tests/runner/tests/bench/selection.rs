use super::{Candidate, Selection};
use crate::cargo::{Image, Kind};
use std::path::PathBuf;

fn candidate(kind: Kind, purpose: &str, target: &str) -> Candidate {
    Candidate {
        cargo: Image {
            package: "fixture".into(),
            target: target.into(),
            kind,
            manifest: PathBuf::from("/compiled/Cargo.toml"),
            image: PathBuf::from(format!("/compiled/{target}")),
        },
        image_sha256: "a".repeat(64),
        architecture: "riscv64".into(),
        purpose: purpose.into(),
    }
}

#[test]
fn only_measurement_purpose_is_selected() {
    let mut selection = Selection {
        schema: 1,
        candidates: vec![
            candidate(Kind::Test, "test", "ordinary"),
            candidate(Kind::Test, "bench", "shared"),
            candidate(Kind::Bench, "bench", "dedicated"),
        ],
    };
    let selected = selection.selected().unwrap();
    assert_eq!(selected.len(), 2);
    assert!(selected.contains_key("shared"));
    assert!(selected.contains_key("dedicated"));
    assert!(!selected.contains_key("ordinary"));

    selection
        .candidates
        .push(candidate(Kind::Bench, "test", "invalid"));
    assert!(selection.selected().is_err());
    selection.candidates.pop();
    selection
        .candidates
        .push(candidate(Kind::Test, "bench", "shared"));
    assert!(selection.selected().is_err());
    selection.candidates = vec![candidate(Kind::Test, "test", "ordinary")];
    assert!(selection.selected().is_err());
}
