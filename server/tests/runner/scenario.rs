use crate::{
    composition, qmp,
    suite::{self, Board, QemuHardware, Run, Suite},
    verify,
};
use clap::Args;
use kernel_verification::{process, *};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const GUEST_CPUS: u8 = 4;

#[derive(Args)]
pub struct Inputs {
    #[arg(long)]
    suite: PathBuf,
    #[arg(long)]
    kernel: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "debug", value_parser = ["debug", "release"])]
    profile: String,
    #[arg(long, value_enum)]
    board: Option<Board>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=100))]
    rounds: u8,
}

struct Variant {
    root: PathBuf,
    elf: PathBuf,
    harts: u8,
    hardware: QemuHardware,
}

fn prepare(args: &Inputs, suite: &Suite, relative: &Path, root: &Path) -> Result<Variant> {
    let Run::Qemu {
        kernel,
        hardware,
        harts,
        imports,
        features,
        environment,
        ..
    } = &suite.run
    else {
        return Err("QEMU provider requested for a non-QEMU suite".into());
    };
    fresh(root)?;
    let provider = root.join("provider");
    let source = revision(&args.kernel)?;
    archive(&args.kernel, &source, &provider)?;
    let patches = root.join("patches");
    fs::create_dir(&patches)?;
    let owner = composition::suite();
    let project = owner.join(relative);
    let contract = project.join("provider.json");
    let patch = project.join("provider.patch");
    require(
        contract.exists() == patch.exists(),
        "a provider patch needs its matching source contract",
    )?;
    if patch.exists() {
        composition::apply(&project, &provider, &patches)?;
    }

    let mut test_sha256 = BTreeMap::new();
    for path in std::iter::once(relative.to_path_buf()).chain(imports.iter().map(PathBuf::from)) {
        let destination = provider.join("server/tests").join(&path);
        let hashes = composition::import(&path, &destination)?;
        test_sha256.insert(path, hashes);
    }
    let suite_sha256 = test_sha256.remove(relative).ok_or("missing suite inputs")?;

    let manifest = provider
        .join("server/tests")
        .join(relative)
        .join("Cargo.toml");
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .current_dir(&provider)
        .env("RUSTFLAGS", FLAGS)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    command
        .arg("--manifest-path")
        .arg(&manifest)
        .args(["--locked", "--target", TARGET]);
    if args.profile == "release" {
        command.arg("--release");
    }

    let qemu = std::env::var("QEMU_RISCV64").unwrap_or("qemu-system-riscv64".into());
    let firmware = Path::new(&std::env::var("QEMU_OPENSBI")?).canonicalize()?;
    let harts = harts.map_or(GUEST_CPUS, |count| count.get());
    let mut inputs = serde_json::json!({
        "source": source,
        "suite": suite.id,
        "profile": args.profile,
        "harts": harts,
        "hardware": hardware,
        "hardware_arguments": hardware.arguments(),
        "tools": tools(&provider, &qemu, &firmware)?,
        "suite_sha256": suite_sha256,
        "imports_sha256": test_sha256,
        "builds": [format!("{command:?}")],
    });
    let lock = manifest.with_file_name("Cargo.lock");
    json(&root.join("inputs.json"), &inputs)?;
    process::build(&mut command, &root.join("locked"))?;
    fs::copy(&lock, root.join("Cargo.lock"))?;

    let binaries = manifest
        .parent()
        .unwrap()
        .join("target")
        .join(TARGET)
        .join(&args.profile);
    let elf = root.join("platform.elf");
    fs::copy(binaries.join(kernel), &elf)?;
    inputs["sha256"] = serde_json::json!({
        "platform.elf": digest(&elf)?,
        "Cargo.lock": digest(&root.join("Cargo.lock"))?,
    });
    json(&root.join("inputs.json"), &inputs)?;
    Ok(Variant {
        root: root.to_owned(),
        elf,
        harts,
        hardware: hardware.clone(),
    })
}

fn run_image(
    elf: &Path,
    evidence: &Path,
    harts: u8,
    hardware: &QemuHardware,
) -> Result<(process::Outcome, std::io::Result<qmp::Shutdown>)> {
    fresh(evidence)?;
    let firmware = Path::new(&std::env::var("QEMU_OPENSBI")?).canonicalize()?;
    let qemu = std::env::var("QEMU_RISCV64").unwrap_or("qemu-system-riscv64".into());
    let mut command = Command::new(qemu);
    command
        .args(["-accel", "tcg,thread=multi"])
        .args(hardware.arguments())
        .arg("-smp")
        .arg(harts.to_string())
        .args(["-m", "128M", "-monitor", "none", "-nic", "none", "-bios"])
        .arg(firmware)
        .arg("-kernel")
        .arg(elf);
    qmp::run(command, evidence, Duration::from_secs(120))
}

/// Execute an already-linked image without interpreting its guest outcome.
/// The scenario owner, not the launcher, decides whether the evidence passes.
pub fn observe(elf: PathBuf, evidence: PathBuf) -> Result<()> {
    let elf = elf.canonicalize()?;
    let (result, shutdown) = run_image(&elf, &evidence, GUEST_CPUS, &QemuHardware::default())?;
    let mut value = serde_json::to_value(&result)?;
    value["qmp_shutdown"] = match shutdown {
        Ok(receipt) => serde_json::to_value(receipt)?,
        Err(error) => serde_json::json!({"error": error.to_string()}),
    };
    value["platform_sha256"] = digest(&elf)?.into();
    json(&evidence.join("observation.json"), &value)
}

fn boot(args: &Inputs, suite: &Suite, variant: &Variant, round: u8) -> Result<&'static str> {
    let Run::Qemu {
        expects, distinct, ..
    } = &suite.run
    else {
        return Err("non-QEMU suite cannot boot".into());
    };
    let harts = variant.harts;
    let evidence = variant.root.join(format!("harts-{harts}-round-{round}"));
    let (mut result, shutdown) = run_image(&variant.elf, &evidence, harts, &variant.hardware)?;
    let log = fs::read(evidence.join("serial.log"))?;
    let details = match std::str::from_utf8(&log) {
        Ok(log) => verify::check(log, expects, distinct.as_ref())?,
        Err(error) => serde_json::json!({"passed": false, "serial_error": error.to_string()}),
    };
    let verdict = verify::verdict(&result, &details, shutdown.as_ref().ok());
    result.passed = verdict == "pass";
    let mut value = serde_json::to_value(&result)?;
    value["scenario"] = details;
    value["qmp_shutdown"] = match shutdown {
        Ok(receipt) => serde_json::to_value(receipt)?,
        Err(error) => serde_json::json!({"error": error.to_string()}),
    };
    value["verdict"] = verdict.into();
    value["platform_sha256"] = digest(&variant.elf)?.into();
    json(&evidence.join("result.json"), &value)?;
    println!(
        "{} {} {} harts={harts} round={round}: {}",
        verdict.to_ascii_uppercase(),
        suite.id,
        args.profile,
        evidence.display()
    );
    Ok(verdict)
}

fn summarize(output: &Path, cases: BTreeMap<String, &'static str>) -> Result<()> {
    let verdict = ["fail", "timeout", "inconclusive"]
        .into_iter()
        .find(|candidate| cases.values().any(|verdict| verdict == candidate))
        .unwrap_or("pass");
    json(
        &output.join("result.json"),
        &serde_json::json!({"verdict": verdict, "cases": cases}),
    )?;
    require(
        verdict == "pass",
        format!("{verdict}: evidence at {}", output.display()),
    )
}

fn record_process(output: &Path, mut process: process::Outcome, verdict: &str) -> Result<()> {
    process.passed = verdict == "pass";
    let mut receipt = serde_json::to_value(&process)?;
    receipt["verdict"] = verdict.into();
    json(&output.join("result.json"), &receipt)
}

fn cargo_test(
    args: &Inputs,
    relative: &Path,
    package: Option<&str>,
    manifest: Option<&str>,
    test: &str,
    ignored: bool,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command.args(["test", "--locked"]);
    if let Some(package) = package {
        command.args(["-p", package]);
    }
    if let Some(manifest) = manifest {
        command.arg("--manifest-path").arg(
            args.kernel
                .join("server/tests")
                .join(relative)
                .join(manifest),
        );
    }
    command
        .args(["--test", test, "--target", "x86_64-unknown-linux-gnu"])
        .current_dir(&args.kernel);
    if ignored {
        command.args(["--", "--ignored", "--nocapture"]);
    }
    let output = args.output.join("cargo");
    let result = process::run(&mut command, &output, Duration::from_secs(900))?;
    let verdict = match result.status.as_str() {
        "timeout" => "timeout",
        "exited" if result.success() => "pass",
        "exited" => "fail",
        _ => "inconclusive",
    };
    record_process(&output, result, verdict)?;
    summarize(&args.output, BTreeMap::from([("cargo".into(), verdict)]))
}

pub fn run(args: Inputs) -> Result<()> {
    let (suite, relative) = suite::read(&args.suite)?;
    require(
        suite.profiles.contains(&args.profile),
        "profile not supported by this test project",
    )?;
    if let Some(board) = args.board {
        require(
            matches!(&suite.run, Run::BoardBoot { boards, .. } if boards.contains(&board)),
            "board not supported by this suite",
        )?;
    }
    let mut args = args;
    args.kernel = args.kernel.canonicalize()?;
    fresh(&args.output)?;
    args.output = args.output.canonicalize()?;
    match &suite.run {
        Run::BoardBoot {
            boards,
            harts,
            launcher,
            ..
        } => {
            let harts = harts.map_or(GUEST_CPUS, |count| count.get());
            require(!boards.is_empty(), "expected boot boards")?;
            let source = revision(&args.kernel)?;
            let provider = args.output.join("provider");
            archive(&args.kernel, &source, &provider)?;
            let project = provider.join("server/tests").join(relative);
            let script = match launcher {
                Some(launcher) => {
                    let target = provider.join("server/tests/target").canonicalize()?;
                    let script = target.join(launcher).canonicalize()?;
                    require(
                        script.starts_with(&target) && script.is_file(),
                        "archived launcher must be in the target test tree",
                    )?;
                    script
                }
                None => project.join("run.sh"),
            };
            let declaration = project.join("suite.json");
            let declaration_sha256 = digest(&declaration)?;
            require(
                declaration_sha256 == digest(&args.suite.join("suite.json"))?,
                "archived suite differs from the checked scenario declaration",
            )?;
            json(
                &args.output.join("inputs.json"),
                &serde_json::json!({
                    "source": source,
                    "suite": suite.id,
                    "profile": args.profile,
                    "harts": harts,
                    "launcher": script,
                    "launcher_sha256": digest(&script)?,
                    "suite_sha256": declaration_sha256,
                    "rustc": output(Command::new("rustc").current_dir(&provider).arg("-Vv"))?,
                    "cargo": output(Command::new("cargo").current_dir(&provider).arg("-V"))?,
                }),
            )?;
            let mut cases = BTreeMap::new();
            for board in boards
                .iter()
                .filter(|board| args.board.is_none_or(|want| want == **board))
            {
                for round in 0..args.rounds {
                    let label = format!("{}-{round}", board.name());
                    let evidence = args.output.join(&label);
                    let mut command = Command::new("bash");
                    command
                        .current_dir(&provider)
                        .arg(&script)
                        .arg(board.name())
                        .arg(&args.profile)
                        .arg(&evidence)
                        .arg(harts.to_string());
                    if launcher.is_some() {
                        command.arg(&suite.id).arg(&declaration);
                    }
                    let launcher_output = args.output.join(format!("{label}-runner"));
                    let result =
                        process::run(&mut command, &launcher_output, Duration::from_secs(600));
                    let verdict = match result {
                        Ok(result) => {
                            let verdict = verify::board_verdict(
                                &result,
                                &evidence,
                                launcher.as_ref().map(|_| (suite.id.as_str(), board.name())),
                            );
                            record_process(&launcher_output, result, verdict)?;
                            verdict
                        }
                        Err(error) => {
                            eprintln!("{label}: {error}");
                            "inconclusive"
                        }
                    };
                    cases.insert(label, verdict);
                }
            }
            summarize(&args.output, cases)
        }
        Run::CargoTest {
            package,
            manifest,
            test,
            ignored,
        } => cargo_test(
            &args,
            &relative,
            package.as_deref(),
            manifest.as_deref(),
            test,
            *ignored,
        ),
        Run::Qemu { .. } => {
            let variant = prepare(&args, &suite, &relative, &args.output.join("candidate"))?;
            let mut cases = BTreeMap::new();
            for round in 0..args.rounds {
                let verdict = match boot(&args, &suite, &variant, round) {
                    Ok(verdict) => verdict,
                    Err(error) => {
                        eprintln!("{} round {round}: {error}", suite.id);
                        "inconclusive"
                    }
                };
                cases.insert(format!("harts-{}-round-{round}", variant.harts), verdict);
            }
            summarize(&args.output, cases)
        }
    }
}
