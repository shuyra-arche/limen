//! T0 experiment: use native libtest to check the cross-process test contract.
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

fn run(case: &str, passed: bool) -> (Value, PathBuf, String) {
    let architecture = env::var("LIMEN_T0_ARCH").expect("select T0 architecture");
    let profile = env::var("LIMEN_T0_PROFILE").expect("select T0 profile");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let (target, mut manifest) = match architecture.as_str() {
        "riscv64" => (
            "riscv64gc-unknown-none-elf",
            "server/tests/target/metal/interrupt/riscv64/ipi/Cargo.toml",
        ),
        "aarch64" => (
            "aarch64-unknown-uefi",
            "server/tests/target/entry/platforms/Cargo.toml",
        ),
        "x86_64" => (
            "x86_64-unknown-uefi",
            "server/tests/target/entry/platforms/Cargo.toml",
        ),
        _ => panic!("unsupported T0 architecture: {architecture}"),
    };
    let discovery = case == "discovery-panic";
    if discovery {
        manifest = "server/tests/target/Cargo.toml";
    }
    let evidence = root.join(".integration/t0").join(case);
    let logs = root.join(".integration/t0/cargo").join(case);
    fs::create_dir_all(&logs).unwrap();
    assert!(!evidence.exists(), "T0 requires fresh evidence");
    let mut runner = vec![env!("CARGO_BIN_EXE_verify"), "native"];
    if case == "reset" {
        runner.push("--observe");
    }
    runner.push(&architecture);
    let config = format!(
        "target.{target}.runner={}",
        serde_json::to_string(&runner).unwrap()
    );
    let command = || {
        let mut cargo = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        cargo.current_dir(&root).args(["--config", &config]);
        if discovery {
            cargo.args(["-Z", "panic-abort-tests"]);
        }
        cargo
            .args(["test", "--locked", "--manifest-path"])
            .arg(root.join(manifest))
            .args(["--target", target])
            .env("LIMEN_T0_CASE", case)
            .env("LIMEN_T0_EVIDENCE", &evidence);
        if discovery {
            cargo.args(["-p", "kernel-guest", "--test", "sync"]);
        } else {
            cargo.args(["--test", "outcome-probe"]);
        }
        match profile.as_str() {
            "debug" => {}
            "release" => {
                cargo.arg("--release");
            }
            _ => panic!("unsupported T0 profile: {profile}"),
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
        passed,
        "{case}: unexpected Cargo exit: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let result = receipt(&evidence.join("result.json"));
    assert_eq!(result["passed"], passed, "{case}: {result}");
    let cleanup = receipt(&evidence.join("cleanup.json"));
    assert_eq!(cleanup["settled"], true);
    assert_eq!(cleanup["unexpected_descendants"], false);
    assert!(cleanup["error"].is_null());
    (result, evidence, architecture)
}

fn exited(result: &Value, code: i32) {
    assert_eq!(result["status"], "exited", "{result}");
    assert_eq!(result["code"], code, "{result}");
}

fn ordinary(architecture: &str) -> i32 {
    if architecture == "x86_64" { 33 } else { 0 }
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn accepts_completed_guest() {
    let (result, _, _) = run("pass", true);
    exited(&result, 97);
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_guest_panic() {
    let (result, evidence, _) = run("panic", false);
    exited(&result, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("T0 intentional assertion failure"));
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_ordinary_shutdown() {
    let (result, _, architecture) = run("shutdown", false);
    exited(&result, ordinary(&architecture));
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_fake_pass_log() {
    let (result, _, architecture) = run("fake-log", false);
    exited(&result, ordinary(&architecture));
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_illegal_instruction() {
    let (result, evidence, _) = run("trap", false);
    assert_eq!(result["status"], "exited");
    let code = result["code"].as_i64().expect("guest exit code");
    assert_ne!(code, 97);
    assert_ne!(code, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("FAIL Limen boot trap:"));
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_timeout() {
    let (result, _, _) = run("timeout", false);
    assert_eq!(result["status"], "timeout");
    assert!(result["code"].is_null());
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn accepts_minor_completion() {
    let (result, _, _) = run("minor-pass", true);
    exited(&result, 97);
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_minor_panic() {
    let (result, evidence, _) = run("minor-panic", false);
    exited(&result, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("T0 intentional minor assertion failure"));
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_guest_reset() {
    let (result, evidence, _) = run("reset", false);
    exited(&result, 0);
    let shutdown = receipt(&evidence.join("shutdown.json"));
    assert_eq!(shutdown["guest"], true);
    assert_eq!(shutdown["reason"], "guest-reset");
}

#[test]
#[ignore = "requires the pinned target toolchain and QEMU"]
fn rejects_collected_panic() {
    let (result, evidence, _) = run("discovery-panic", false);
    exited(&result, 99);
    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    assert!(serial.contains("T1 intentional collected test failure"));
}
