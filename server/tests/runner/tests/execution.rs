use crate::qmp;
use kernel_verification::process;
use std::{fs, process::Command, thread, time::Duration};
fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("kernel-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    root
}

#[test]
fn qmp_records_one_shutdown_after_negotiating_and_starting_the_guest() {
    let startup = concat!(
        "{\"QMP\":{}}\r\n",
        "{\"return\":{},\"id\":\"capabilities\"}\n",
        "{\"event\":\"RESUME\"}\n",
        "{\"return\":{},\"id\":\"start\"}\n",
    );
    let event = r#"{"event":"SHUTDOWN","data":{"guest":true,"reason":"guest-shutdown"}}"#;
    let input = format!("{startup}{event}\n");
    let mut commands = Vec::new();
    let mut transcript = Vec::new();
    let receipt = qmp::capture(
        input.as_bytes(),
        &mut commands,
        &mut transcript,
        None::<fn() -> std::io::Result<()>>,
    )
    .unwrap();
    assert!(receipt.guest);
    assert_eq!(receipt.reason, "guest-shutdown");
    assert_eq!(transcript, input.as_bytes());
    let commands: Vec<serde_json::Value> = String::from_utf8(commands)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["execute"], "qmp_capabilities");
    assert_eq!(commands[1]["execute"], "cont");

    for incomplete in [
        startup.to_string(),
        format!("{startup}{event}"),
        format!("{input}{event}\n"),
        format!("{input}invalid\n"),
        format!("{input}{{}}\n"),
        input.replace("\"id\":\"start\"", "\"id\":\"other\""),
    ] {
        assert!(
            qmp::capture(
                incomplete.as_bytes(),
                Vec::new(),
                Vec::new(),
                None::<fn() -> std::io::Result<()>>,
            )
            .is_err()
        );
    }
}
#[test]
fn qmp_can_observe_execution_owned_by_an_external_controller() {
    let greeting = concat!(
        "{\"QMP\":{}}\n",
        "{\"return\":{},\"id\":\"capabilities\"}\n",
    );
    let resumed = "{\"event\":\"RESUME\"}\n";
    let stopped = "{\"event\":\"STOP\"}\n";
    let shutdown = concat!(
        "{\"event\":\"SHUTDOWN\",",
        "\"data\":{\"guest\":true,\"reason\":\"guest-shutdown\"}}\n",
    );
    let input = format!("{greeting}{resumed}{stopped}{resumed}{shutdown}");
    let mut commands = Vec::new();
    let mut invoked = false;
    let result = qmp::capture(
        input.as_bytes(),
        &mut commands,
        Vec::new(),
        Some(|| {
            invoked = true;
            Ok(())
        }),
    )
    .unwrap();
    assert!(invoked && result.guest);
    assert_eq!(String::from_utf8(commands).unwrap().lines().count(), 1);

    let without_resume = format!("{greeting}{shutdown}");
    assert!(
        qmp::capture(
            without_resume.as_bytes(),
            Vec::new(),
            Vec::new(),
            Some(|| Ok(())),
        )
        .is_err()
    );
    assert!(
        qmp::capture(
            input.as_bytes(),
            Vec::new(),
            Vec::new(),
            Some(|| Err(std::io::Error::other("controller failed"))),
        )
        .is_err()
    );
}

#[test]
fn process_reports_exit_and_failure_without_interpreting_stdout() {
    let root = scratch("exit");
    for (name, script, status, code) in [
        ("good", "echo PASS", "exited", Some(0)),
        ("failed", "echo PASS; exit 3", "exited", Some(3)),
        ("signal", "echo PASS; kill -TERM $$", "signal", None),
    ] {
        let result = process::run(
            Command::new("sh").args(["-c", script]),
            &root.join(name),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(result.status, status);
        assert_eq!(result.code, code);
        assert_eq!(result.success(), code == Some(0));
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
    assert_eq!(result.status, "timeout");
    assert!(!result.success());
    assert!(result.elapsed_seconds < 5.0);
    let cleanup: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("cleanup.json")).unwrap()).unwrap();
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
    let cleanup: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("cleanup.json")).unwrap()).unwrap();
    assert_eq!(result.code, Some(0));
    assert_eq!(result.status, "cleanup-failed");
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
    assert!(result.is_err());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("result.json")).unwrap()).unwrap();
    let cleanup: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("cleanup.json")).unwrap()).unwrap();
    assert_eq!(receipt["status"], "exited");
    assert_eq!(receipt["code"], 2);
    assert_eq!(receipt["passed"], false);
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
fn failed_launch_preserves_a_failed_result() {
    let root = scratch("spawn");
    assert!(
        process::run(
            &mut Command::new("/nonexistent/vanllia-qemu"),
            &root,
            Duration::from_secs(1)
        )
        .is_err()
    );
    let result: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("result.json")).unwrap()).unwrap();
    assert_eq!(result["status"], "spawn-error");
    assert_eq!(result["passed"], false);
    let cleanup: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("cleanup.json")).unwrap()).unwrap();
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
