//! Real hardware events through the installed long-mode terminal IST paths.
#![cfg(limen_guest = "x86_64")]

use serde_json::Value;
use std::{env, fs, path::Path, process::Command};

fn hex(text: &str, field: &str) -> usize {
    let value = text
        .split_whitespace()
        .find_map(|entry| entry.strip_prefix(field))
        .unwrap_or_else(|| panic!("missing {field} in {text}"));
    usize::from_str_radix(value.strip_prefix("0x").unwrap(), 16).unwrap()
}

fn run(case: &str, expected_prefix: &str, cause: usize) -> (String, String) {
    let target = env::var("LIMEN_TEST_TARGET").unwrap();
    assert_eq!(target, "x86_64-unknown-uefi");
    let profile = env::var("LIMEN_TEST_PROFILE").unwrap();
    let runner = env::var("LIMEN_TEST_RUNNER").unwrap();
    let config = format!(
        "target.{target}.runner={}",
        serde_json::to_string(&[runner.as_str(), "native"]).unwrap()
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let output = root.join(".integration/x86-runtime").join(&profile);
    let evidence = output.join(case);
    let logs = output.join("cargo").join(case);
    fs::create_dir_all(&logs).unwrap();
    assert!(!evidence.exists(), "the observer requires fresh evidence");
    let cargo = || {
        let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command
            .current_dir(&root)
            .args(["--config", &config, "-Z", "panic-abort-tests"])
            .args(["test", "--locked", "--manifest-path"])
            .arg(root.join("server/tests/target/Cargo.toml"))
            .args(["-p", "x86_64-guest", "--test", "ipi", "--target", &target])
            .env("LIMEN_X86_TRAP_CASE", case)
            .env("LIMEN_TEST_EVIDENCE", &evidence);
        match profile.as_str() {
            "debug" => {}
            "release" => {
                command.arg("--release");
            }
            _ => panic!("unsupported guest profile: {profile}"),
        }
        command
    };
    let build = cargo().arg("--no-run").output().unwrap();
    fs::write(logs.join("build.stdout"), &build.stdout).unwrap();
    fs::write(logs.join("build.stderr"), &build.stderr).unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = cargo().output().unwrap();
    fs::write(logs.join("run.stdout"), &run.stdout).unwrap();
    fs::write(logs.join("run.stderr"), &run.stderr).unwrap();
    assert!(!run.status.success());

    let process: Value =
        serde_json::from_slice(&fs::read(evidence.join("process.json")).unwrap()).unwrap();
    let result: Value =
        serde_json::from_slice(&fs::read(evidence.join("result.json")).unwrap()).unwrap();
    // QEMU's debug exit translates the terminal's 0x11 into 35.
    assert_eq!(process["termination"]["kind"], "exited");
    assert_eq!(process["termination"]["value"], 35);
    assert_eq!(process["cleanup"]["settled"], true);
    assert_eq!(process["cleanup"]["unexpected_descendants"], false);
    assert!(process["cleanup"]["error"].is_null());
    assert_eq!(result["verdict"], "fail");

    let serial = fs::read_to_string(evidence.join("serial.log")).unwrap();
    let expected = serial
        .lines()
        .find(|line| line.starts_with(expected_prefix))
        .expect("the guest published the exact hardware trigger");
    let actual = serial
        .lines()
        .find(|line| line.starts_with("FAIL Limen boot trap:"))
        .unwrap_or_else(|| panic!("native terminal evidence missing: {serial}"));
    assert_eq!(hex(actual, "stack="), hex(expected, "stack="));
    assert_eq!(hex(actual, "cause="), cause);
    assert_eq!(hex(actual, "status="), 0);
    (expected.to_owned(), actual.to_owned())
}

#[test]
fn terminates_an_actual_nmi_on_an_unusable_interrupted_stack() {
    let (expected, actual) = run("nmi", "EXPECT Limen NMI:", 2);
    let pc = hex(&actual, "pc=");
    assert!((hex(&expected, "first=")..hex(&expected, "limit=")).contains(&pc));
}

#[test]
fn terminates_a_real_double_fault_when_gp_delivery_also_faults() {
    run("double-fault", "EXPECT Limen double fault:", 8);
}
