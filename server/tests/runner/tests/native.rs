use super::{Verdict, judge};
use crate::process;
use std::{fs, path::PathBuf, process::Command, time::Duration};

fn scratch(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("kernel-native-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    root
}

#[test]
fn verdict_is_separate_from_process_termination() {
    let root = scratch("verdict");
    for (code, expected) in [
        (97, Verdict::Pass),
        (99, Verdict::Fail),
        (0, Verdict::Fail),
    ] {
        let outcome = process::run(
            Command::new("sh").args(["-c", &format!("echo PASS; exit {code}")]),
            &root.join(code.to_string()),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(judge(&outcome, true).verdict, expected);
        assert_eq!(judge(&outcome, false).verdict, Verdict::Inconclusive);
        assert_eq!(outcome.termination, process::Termination::Exited(code));
        assert!(!root.join(code.to_string()).join("result.json").exists());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incomplete_execution_is_not_a_guest_failure_or_success() {
    let root = scratch("unavailable");
    let outcome = process::run(
        &mut Command::new("/nonexistent/limen-guest"),
        &root,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(judge(&outcome, true).verdict, Verdict::Inconclusive);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn timeout_remains_distinct_from_an_assertion_failure() {
    let root = scratch("timeout");
    let outcome = process::run(
        Command::new("sh").args(["-c", "echo PASS; exec sleep 30"]),
        &root,
        Duration::from_millis(100),
    )
    .unwrap();
    assert_eq!(judge(&outcome, true).verdict, Verdict::Timeout);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn explicit_guest_exit_cannot_override_unexpected_descendants() {
    let root = scratch("descendants");
    let outcome = process::run(
        Command::new("sh").args(["-c", "sleep 30 & exit 97"]),
        &root,
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(outcome.termination, process::Termination::Exited(97));
    assert_eq!(judge(&outcome, true).verdict, Verdict::Inconclusive);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn preparation_error_records_inconclusive_without_a_process_receipt() {
    let root = scratch("preparation");
    let image = root.join("invalid-image");
    fs::write(&image, b"not a guest executable").unwrap();
    let evidence = root.join("evidence");
    assert!(super::run(&image, false, Some(&evidence)).is_err());
    let directory = evidence.join("invalid-image");
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["verdict"], "inconclusive");
    assert!(result["reason"].is_string());
    assert!(!directory.join("process.json").exists());
    fs::remove_dir_all(root).unwrap();
}
