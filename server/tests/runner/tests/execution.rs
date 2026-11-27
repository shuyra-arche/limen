use kernel_verification::process::{self, Termination};
use std::{fs, process::Command, thread, time::Duration};
fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("kernel-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    root
}

#[test]
fn process_reports_exit_and_failure_without_interpreting_stdout() {
    let root = scratch("exit");
    for (name, script, termination) in [
        ("good", "echo PASS", Termination::Exited(0)),
        ("failed", "echo PASS; exit 3", Termination::Exited(3)),
        (
            "signal",
            "echo PASS; kill -TERM $$",
            Termination::Signaled(libc::SIGTERM),
        ),
    ] {
        let result = process::run(
            Command::new("sh").args(["-c", script]),
            &root.join(name),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(result.termination, termination);
        assert_eq!(result.success(), termination == Termination::Exited(0));
        assert_eq!(
            fs::read_to_string(root.join(name).join("serial.log")).unwrap(),
            "PASS\n"
        );
    }
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn deadline_kills_even_after_success_marker() {
    let root = scratch("deadline");
    let result = process::run(
        Command::new("sh").args(["-c", "echo PASS; sleep 30"]),
        &root,
        Duration::from_millis(100),
    )
    .unwrap();
    assert_eq!(result.termination, Termination::Timeout);
    assert!(!result.success());
    assert!(result.elapsed_seconds < 5.0);
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("process.json")).unwrap()).unwrap();
    let cleanup = &receipt["cleanup"];
    assert_eq!(cleanup["settled"], true);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn successful_leader_cannot_leave_a_background_process() {
    let root = scratch("background");
    let result = process::run(
        Command::new("sh").args(["-c", "sleep 30 & echo PASS; exit 0"]),
        &root,
        Duration::from_secs(2),
    )
    .unwrap();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("process.json")).unwrap()).unwrap();
    let cleanup = &receipt["cleanup"];
    assert_eq!(result.termination, Termination::Exited(0));
    assert!(!result.clean());
    assert!(!result.success());
    assert_eq!(cleanup["unexpected_descendants"], true);
    assert_eq!(cleanup["killed"], true);
    assert_eq!(cleanup["settled"], true);
    assert!(cleanup["reaped"].as_u64().unwrap() >= 1);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn nonzero_exit_with_a_background_process_is_not_a_clean_business_result() {
    let root = scratch("background-nonzero");
    let result = process::run(
        Command::new("sh").args(["-c", "sleep 30 & echo PASS; exit 2"]),
        &root,
        Duration::from_secs(2),
    );
    let result = result.unwrap();
    assert_eq!(result.termination, Termination::Exited(2));
    assert!(!result.clean());
    assert!(!result.success());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("process.json")).unwrap()).unwrap();
    assert_eq!(receipt["termination"]["kind"], "exited");
    assert_eq!(receipt["termination"]["value"], 2);
    let cleanup = &receipt["cleanup"];
    assert_eq!(cleanup["unexpected_descendants"], true);
    assert_eq!(cleanup["settled"], true);
    assert!(cleanup["reaped"].as_u64().unwrap() >= 1);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn large_output_cannot_deadlock_pipes() {
    let root = scratch("streams");
    let result = process::run(
        Command::new("sh").args([
            "-c",
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
        ]),
        &root,
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(result.success());
    assert_eq!(
        fs::metadata(root.join("serial.log")).unwrap().len(),
        1048576
    );
    assert_eq!(
        fs::metadata(root.join("stderr.log")).unwrap().len(),
        1048576
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn failed_launch_preserves_the_unstarted_process_receipt() {
    let root = scratch("spawn");
    let outcome = process::run(
        &mut Command::new("/nonexistent/vanllia-qemu"),
        &root,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(outcome.termination, Termination::NotStarted);
    assert!(!outcome.clean());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("process.json")).unwrap()).unwrap();
    assert_eq!(receipt["termination"]["kind"], "not-started");
    assert!(receipt["error"].is_string());
    let cleanup = &receipt["cleanup"];
    assert_eq!(cleanup["spawned"], false);
    assert_eq!(cleanup["settled"], true);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn runner_cancellation_signals_the_direct_child() {
    if let Some(marker) = std::env::var_os("KERNEL_VERIFICATION_PARENT_DEATH_MARKER") {
        let child_dir = std::env::var_os("KERNEL_VERIFICATION_CHILD_DIR")
            .expect("parent passes the child log directory");
        let child_dir = std::path::PathBuf::from(child_dir);
        let mut command = Command::new("sh");
        command.args([
            "-c",
            r#"echo child-started; sleep 3; touch "$KERNEL_VERIFICATION_PARENT_DEATH_MARKER""#,
        ]);
        command.env("KERNEL_VERIFICATION_PARENT_DEATH_MARKER", marker);
        let _ = process::run(&mut command, &child_dir, Duration::from_secs(10));
        return;
    }

    let root = scratch("cancel-runner");
    let marker = root.join("orphan-finished");
    let child_dir = root.join("child");
    let mut test_child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "execution::runner_cancellation_signals_the_direct_child",
        ])
        .env("KERNEL_VERIFICATION_PARENT_DEATH_MARKER", &marker)
        .env("KERNEL_VERIFICATION_CHILD_DIR", &child_dir)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if fs::read_to_string(child_dir.join("serial.log"))
            .is_ok_and(|serial| serial.contains("child-started"))
        {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        fs::read_to_string(child_dir.join("serial.log"))
            .is_ok_and(|serial| serial.contains("child-started")),
        "process helper child did not start"
    );
    test_child.kill().unwrap();
    test_child.wait().unwrap();
    thread::sleep(Duration::from_millis(3_200));
    assert!(
        !marker.exists(),
        "direct child continued after runner cancellation"
    );
    fs::remove_dir_all(root).unwrap();
}
