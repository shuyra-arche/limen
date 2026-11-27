//! Native libtest checks the cross-process completion contract.
//! These are infrastructure tests, not the kernel's ordinary hardware tests.

use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn receipt(path: &Path) -> Value {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("missing receipt {}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap()
}

fn cargo_logs(output: &Output, directory: &Path, name: &str) {
    fs::write(directory.join(format!("{name}.stdout")), &output.stdout).unwrap();
    fs::write(directory.join(format!("{name}.stderr")), &output.stderr).unwrap();
}

fn run(case: &str, verdict: &str) -> (Value, PathBuf, String) {
    let architecture = env::var("LIMEN_TEST_ARCH").expect("select guest architecture");
    let profile = env::var("LIMEN_TEST_PROFILE").expect("select guest profile");
    let target = env::var("LIMEN_TEST_TARGET").expect("select guest target");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let runner = PathBuf::from(env::var_os("LIMEN_TEST_RUNNER").expect("provide built runner"))
        .canonicalize()
        .unwrap();
    let runner = runner.to_str().expect("UTF-8 runner path");
    let test = if case == "discovery-panic" {
        "sync"
    } else {
        "outcome"
    };
    let evidence = root.join(".integration/transport").join(case);
    let logs = root.join(".integration/transport/cargo").join(case);
    fs::create_dir_all(&logs).unwrap();
    assert!(!evidence.exists(), "transport requires fresh evidence");
    let mut runner = vec![runner, "native"];
    if case == "reset" {
        runner.push("--observe");
    }
    let config = format!(
        "target.{target}.runner={}",
        serde_json::to_string(&runner).unwrap()
    );
    let command = || {
        let mut cargo = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        cargo
            .current_dir(&root)
            .args(["--config", &config, "-Z", "panic-abort-tests"])
            .args(["test", "--locked", "--manifest-path"])
            .arg(root.join("server/tests/target/Cargo.toml"))
            .args(["-p", "kernel-guest", "--test", test, "--target", &target])
            .env("LIMEN_TEST_CASE", case)
            .env("LIMEN_TEST_EVIDENCE", &evidence);
        match profile.as_str() {
            "debug" => {}
            "release" => {
                cargo.arg("--release");
            }
            _ => panic!("unsupported guest profile: {profile}"),
        }
        cargo
    };
    // A compilation failure is never evidence of an expected guest failure.
    let build = command().arg("--no-run").output().unwrap();
    cargo_logs(&build, &logs, "build");
    assert!(
        build.status.success(),
        "{case}: guest failed to compile: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let output = command().output().unwrap();
    cargo_logs(&output, &logs, "run");
    assert_eq!(
        output.status.success(),
        verdict == "pass",
        "{case}: unexpected Cargo exit: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let result = receipt(&evidence.join("result.json"));
    assert_eq!(result["verdict"], verdict, "{case}: {result}");
    let process = receipt(&evidence.join("process.json"));
    let cleanup = &process["cleanup"];
    assert_eq!(cleanup["settled"], true);
    assert_eq!(cleanup["unexpected_descendants"], false);
    assert!(cleanup["error"].is_null());
    (process, evidence, architecture)
}

fn exited(result: &Value, code: i32) {
    assert_eq!(result["termination"]["kind"], "exited", "{result}");
    assert_eq!(result["termination"]["value"], code, "{result}");
}

fn ordinary(architecture: &str) -> i32 {
    if architecture == "x86_64" { 33 } else { 0 }
}

#[test]
fn accepts_completed_guest() {
    let (result, _, _) = run("pass", "pass");
    exited(&result, 97);
}

#[test]
fn rejects_guest_panic() {
    let (result, evidence, _) = run("panic", "fail");
    exited(&result, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("T0 intentional assertion failure"));
}

#[test]
fn rejects_ordinary_shutdown() {
    let (result, _, architecture) = run("shutdown", "fail");
    exited(&result, ordinary(&architecture));
}

#[test]
fn rejects_fake_pass_log() {
    let (result, _, architecture) = run("fake-log", "fail");
    exited(&result, ordinary(&architecture));
}

#[test]
fn rejects_illegal_instruction() {
    let (result, evidence, _) = run("trap", "fail");
    assert_eq!(result["termination"]["kind"], "exited");
    let code = result["termination"]["value"]
        .as_i64()
        .expect("guest exit code");
    assert_ne!(code, 97);
    assert_ne!(code, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("FAIL Limen boot trap:"));
}

#[test]
#[ignore = "full-duration timeout probe; run explicitly"]
fn rejects_timeout() {
    let (result, _, _) = run("timeout", "timeout");
    assert_eq!(result["termination"]["kind"], "timeout");
    assert!(result["termination"]["value"].is_null());
}

#[test]
fn accepts_minor_completion() {
    let (result, _, _) = run("minor-pass", "pass");
    exited(&result, 97);
}

#[test]
fn rejects_minor_panic() {
    let (result, evidence, _) = run("minor-panic", "fail");
    exited(&result, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("T0 intentional minor assertion failure"));
}

#[test]
fn rejects_guest_reset() {
    let (result, evidence, _) = run("reset", "fail");
    exited(&result, 0);
    let shutdown = receipt(&evidence.join("shutdown.json"));
    assert_eq!(shutdown["guest"], true);
    assert_eq!(shutdown["reason"], "guest-reset");
}

#[test]
fn rejects_collected_panic() {
    let (result, evidence, _) = run("discovery-panic", "fail");
    exited(&result, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("T1 intentional collected test failure"));
}
