//! A terminal boot fault cannot execute an in-guest test assertion.
//! Validate the actual linked image and its observed trap from the host.

use object::{Architecture, Object, ObjectSection, ObjectSymbol};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn receipt(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn run(image: &Path, root: &Path, observe: bool) -> PathBuf {
    let mut command = Command::new(env!("CARGO_BIN_EXE_verify"));
    command.args(["native", "--evidence-root"]).arg(root);
    if observe {
        command.arg("--observe");
    }
    let Output {
        status,
        stdout,
        stderr,
    } = command.arg(image).output().unwrap();
    let evidence = root.join(image.file_name().unwrap());
    fs::write(evidence.join("runner.stdout"), stdout).unwrap();
    fs::write(evidence.join("runner.stderr"), stderr).unwrap();
    assert_eq!(status.code(), Some(if observe { 1 } else { 0 }));
    let record = receipt(&evidence.join("platform.json"));
    assert_eq!(
        record["image_sha256"],
        kernel_verification::digest(image).unwrap()
    );
    let cleanup = receipt(&evidence.join("cleanup.json"));
    assert_eq!(cleanup["settled"], true);
    assert_eq!(cleanup["unexpected_descendants"], false);
    assert!(cleanup["error"].is_null());
    evidence
}

fn linked_guest(root: &Path, evidence: &Path) -> PathBuf {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root).args([
        "-Z",
        "panic-abort-tests",
        "test",
        "--locked",
        "--manifest-path",
        "server/tests/target/Cargo.toml",
        "-p",
        "kernel-guest",
        "--test",
        "riscv64-boot-handoff",
        "--target",
        "riscv64gc-unknown-none-elf",
        "--no-run",
        "--message-format=json",
    ]);
    match env::var("LIMEN_TEST_PROFILE").unwrap().as_str() {
        "debug" => {}
        "release" => {
            command.arg("--release");
        }
        _ => panic!("unsupported guest profile"),
    }
    let output = command.output().unwrap();
    fs::write(evidence.join("cargo.stdout"), &output.stdout).unwrap();
    fs::write(evidence.join("cargo.stderr"), &output.stderr).unwrap();
    assert!(output.status.success(), "boot fault guest failed to compile");
    let artifacts: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|message| {
            message["reason"] == "compiler-artifact"
                && message["target"]["name"] == "riscv64-boot-handoff"
                && message["profile"]["test"] == true
        })
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
        .collect();
    assert_eq!(artifacts.len(), 1, "expected the exact linked test image");
    artifacts[0].canonicalize().unwrap()
}

fn inject(bytes: &mut [u8]) -> (usize, u64, u64, u64) {
    let file = object::File::parse(&*bytes).unwrap();
    assert_eq!(file.architecture(), Architecture::Riscv64);
    let symbol = |name: &str| {
        let mut matches = file
            .symbols()
            .filter(|symbol| symbol.name().ok() == Some(name) && !symbol.is_undefined());
        let address = matches.next().unwrap().address();
        assert!(matches.next().is_none(), "ambiguous linked symbol: {name}");
        address
    };
    let start = symbol("_start");
    let bss_start = symbol("_bss_start");
    let bss_end = symbol("_bss_end");
    let major_start = symbol("_major_stack_start");
    let major_top = symbol("_major_stack_end");
    assert!(bss_end.checked_sub(bss_start).unwrap() > 8);
    assert!(bss_start <= major_start && major_start < major_top && major_top <= bss_end);
    assert!(major_start.is_multiple_of(16) && major_top.is_multiple_of(16));
    let text = file.section_by_name(".text").unwrap();
    let (offset, length) = text.file_range().unwrap();
    let relative = start.checked_sub(text.address()).unwrap();
    let end = relative.checked_add(128).unwrap();
    assert!(end <= length && end <= text.size());
    let begin = usize::try_from(offset.checked_add(relative).unwrap()).unwrap();
    let code = &bytes[begin..begin + 128];
    // This exact reviewed BSS clear loop follows the installation of stvec.
    // A different backend encoding requires a review rather than blind patching.
    let clear = [
        0x63, 0x86, 0x62, 0x00, 0x23, 0xb0, 0x02, 0x00, 0xa1, 0x02, 0xdd, 0xbf,
    ];
    let mut matches = code
        .windows(clear.len())
        .enumerate()
        .filter(|(_, window)| *window == clear);
    let (index, _) = matches.next().expect("reviewed BSS clear loop");
    assert!(matches.next().is_none() && index.is_multiple_of(2));
    assert!(
        code[..index]
            .windows(4)
            .any(|window| window == [0x73, 0x90, 0x52, 0x10])
    );
    let delta = index + 8;
    let position = begin + delta;
    assert_eq!(&bytes[position..position + 2], &[0xa1, 0x02]);
    bytes[position..position + 2].fill(0);
    (position, start + delta as u64, major_start, major_top)
}

fn diagnostic(log: &str, pc: u64, stack: u64) {
    assert!(log.ends_with('\n'), "truncated diagnostic");
    let expected = regex::Regex::new(&format!(
        r"^FAIL Limen boot trap: cause=0x2 pc={pc:#x} value=0x0 stack={stack:#x} status=0x[0-9a-f]+$"
    ))
    .unwrap();
    let failures: Vec<_> = log
        .lines()
        .filter(|line| *line == "FAIL" || line.starts_with("FAIL "))
        .collect();
    assert_eq!(failures.len(), 1, "{log}");
    assert!(expected.is_match(failures[0]), "{log}");
    assert!(!log.contains("PASS boot-handoff"));
}

#[test]
#[ignore = "requires the pinned RV64 toolchain and QEMU"]
fn bss_clear_traps_on_the_original_major_stack() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let evidence = root.join(".integration/boot-fault");
    kernel_verification::fresh(&evidence).unwrap();
    let linked = linked_guest(&root, &evidence);
    let original = evidence.join("original.elf");
    fs::copy(&linked, &original).unwrap();
    let hash = kernel_verification::digest(&original).unwrap();
    let machine = crate::platform::read(&original).unwrap();
    assert_eq!(machine.architecture, "riscv64");
    assert_eq!(machine.platform.harts, 4);
    let normal = run(&original, &evidence.join("normal"), false);
    let result = receipt(&normal.join("result.json"));
    assert_eq!(result["status"], "exited");
    assert_eq!(result["code"], 97);
    assert_eq!(result["passed"], true);

    let mut bytes = fs::read(&original).unwrap();
    let (position, pc, major_start, stack) = inject(&mut bytes);
    let fault = evidence.join("fault.elf");
    fs::write(&fault, bytes).unwrap();
    assert_eq!(
        crate::platform::read(&fault).unwrap().platform,
        machine.platform
    );
    assert_eq!(kernel_verification::digest(&original).unwrap(), hash);
    let patched_hash = kernel_verification::digest(&fault).unwrap();
    assert_ne!(hash, patched_hash);
    kernel_verification::json(
        &evidence.join("injection.json"),
        &serde_json::json!({
            "original_sha256": hash,
            "patched_sha256": patched_hash,
            "pc": format!("{pc:#x}"),
            "interrupted_sp": format!("{stack:#x}"),
            "major_stack_start": format!("{major_start:#x}"),
            "file_offset": position,
            "before": "a102",
            "after": "0000",
        }),
    )
    .unwrap();

    let observed = run(&fault, &evidence.join("fault"), true);
    let result = receipt(&observed.join("result.json"));
    assert_eq!(result["status"], "exited");
    assert_eq!(result["code"], 0);
    assert_eq!(result["passed"], false);
    let shutdown = receipt(&observed.join("shutdown.json"));
    assert_eq!(
        shutdown,
        serde_json::json!({"guest": true, "reason": "guest-shutdown"})
    );
    diagnostic(
        &fs::read_to_string(observed.join("serial.log")).unwrap(),
        pc,
        stack,
    );
}
