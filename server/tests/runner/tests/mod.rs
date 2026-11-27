#![allow(dead_code)]

#[path = "../composition/mod.rs"]
mod composition;
#[path = "../qmp.rs"]
mod qmp;
#[path = "../scenario.rs"]
mod scenario;
#[path = "../suite.rs"]
mod suite;
#[path = "../utilities.rs"]
mod utilities;
#[path = "../verify.rs"]
mod verify;

#[test]
fn projects_own_their_manifests_and_verdicts() {
    use std::{collections::BTreeSet, fs, path::Path};
    fn visit(path: &Path, ids: &mut BTreeSet<String>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.join("suite.json").is_file() {
                let (project, _) = suite::read(&path).unwrap();
                assert!(ids.insert(project.id));
            } else if path.is_dir() {
                visit(&path, ids);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
    let mut ids = BTreeSet::new();
    visit(&root, &mut ids);
    assert!(!ids.is_empty());
}

#[test]
fn guest_suites_accept_a_single_hart_and_reject_zero() {
    let manifest = |harts| {
        serde_json::json!({
            "kind": "board-boot",
            "boards": ["q35"],
            "harts": harts,
        })
    };
    let run = serde_json::from_value::<suite::Run>(manifest(1)).unwrap();
    assert!(matches!(
        run,
        suite::Run::BoardBoot { harts: Some(count), .. } if count.get() == 1
    ));
    assert!(serde_json::from_value::<suite::Run>(manifest(0)).is_err());

    let manifest = |harts| {
        serde_json::json!({
            "kind": "qemu", "kernel": "module-riscv", "expects": ["PASS"], "harts": harts,
        })
    };
    let run = serde_json::from_value::<suite::Run>(manifest(1)).unwrap();
    assert!(matches!(
        run,
        suite::Run::Qemu { harts: Some(count), .. } if count.get() == 1
    ));
    assert!(serde_json::from_value::<suite::Run>(manifest(0)).is_err());
}

#[test]
fn runner_accepts_project_paths_without_named_commands() {
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        inputs: scenario::Inputs,
    }
    let args = [
        "verify",
        "--suite",
        "server/tests/target/example",
        "--kernel",
        "/example/kernel",
        "--output",
        "/example/evidence",
    ];
    assert!(Cli::try_parse_from(args).is_ok());
    assert!(Cli::try_parse_from(["verify", "--kernel", "/example/kernel"]).is_err());
}

#[test]
fn provider_patches_fail_closed_on_unreviewed_sources() {
    let root = std::env::temp_dir().join(format!("kernel-provider-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let project = root.join("project");
    let provider = root.join("provider");
    let evidence = root.join("evidence");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&provider).unwrap();
    std::fs::create_dir_all(&evidence).unwrap();
    let original = "before\n";
    let source = provider.join("source.rs");
    std::fs::write(&source, original).unwrap();
    let sha = kernel_verification::digest(&source).unwrap();
    std::fs::write(
        project.join("provider.json"),
        serde_json::to_vec(&serde_json::json!({"source.rs":sha})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        project.join("provider.patch"),
        "--- a/source.rs\n+++ b/source.rs\n@@ -1 +1 @@\n-before\n+after\n",
    )
    .unwrap();
    std::fs::write(&source, "changed\n").unwrap();
    assert!(composition::apply(&project, &provider, &evidence).is_err());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "changed\n");
    std::fs::write(&source, original).unwrap();
    composition::apply(&project, &provider, &evidence).unwrap();
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "after\n");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn generic_verdict_rejects_missing_and_duplicate_evidence() {
    let expected = vec!["PASS completed participants=2".to_owned()];
    let distinct = suite::Distinct {
        pattern: r"OBSERVED hart=(\d+)".to_owned(),
        count: 2,
    };
    let log = "PASS completed participants=2\nOBSERVED hart=1\nOBSERVED hart=3\n";
    assert_eq!(
        verify::check(log, &expected, Some(&distinct)).unwrap()["passed"],
        true
    );
    for invalid in [
        log.replace("hart=3", "hart=1"),
        log.replace("participants=2", "participants=4"),
        format!("{log}FAIL Limen boot: invalid firmware input\n"),
    ] {
        assert_eq!(
            verify::check(&invalid, &expected, Some(&distinct)).unwrap()["passed"],
            false
        );
    }
}

#[cfg(unix)]
#[test]
fn symbolizer_preserves_literal_return_addresses() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("kernel-symbolize-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).unwrap();
    let tool = root.join("addr2line");
    let serial = root.join("serial.log");
    std::fs::write(
        &tool,
        "#!/bin/sh\nprintf '%s\\n' \"$5\" >> \"$4.addresses\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&serial, b"unrelated pc=0x50\nFAIL Limen boot trap: cause=0x2 pc=0x80200040 value=0x0 stack=0x80208000 status=0x100\n").unwrap();
    utilities::symbolize(&root.join("kernel.elf"), &serial, tool.to_str().unwrap()).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("kernel.elf.addresses")).unwrap(),
        "0x80200040\n"
    );
    assert!(utilities::symbolize(&root.join("kernel.elf"), &serial, "/bin/false").is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rv64_hardware_is_declared_by_the_scenario() {
    let manifest = |value: &str| {
        serde_json::json!({
            "kind": "qemu", "kernel": "interrupt", "expects": ["PASS"],
            "hardware": {
                "machine": {"name": "virt", "options": {"aia": "aplic", "aclint": "on"}},
                "cpu": {"name": "rv64", "options": {"sstc": "false"}},
                "devices": [{"name": "example", "options": {"bus": "pcie.0", "addr": value}}],
            }
        })
    };
    let run = serde_json::from_value::<suite::Run>(manifest("06.0")).unwrap();
    let suite::Run::Qemu { hardware, .. } = run else {
        panic!("not a QEMU suite");
    };
    assert!(hardware.valid());
    assert_eq!(
        hardware.arguments(),
        [
            "-machine",
            "virt,aclint=on,aia=aplic",
            "-cpu",
            "rv64,sstc=false",
            "-device",
            "example,addr=06.0,bus=pcie.0",
        ]
    );
    let malformed = serde_json::from_value::<suite::Run>(manifest("06.0,extra=on")).unwrap();
    assert!(matches!(malformed, suite::Run::Qemu { hardware, .. } if !hardware.valid()));
    assert_eq!(
        suite::QemuHardware::default().arguments(),
        ["-machine", "virt", "-cpu", "rv64"]
    );
}

mod execution;
mod transport;
