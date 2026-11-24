use crate::{
    composition, qmp,
    suite::{self, Board, Run, Suite},
    verify,
};
use clap::Args;
use kernel_verification::{process, *};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

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
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(4..=4))]
    harts: u8,
    #[arg(long, value_enum)]
    board: Option<Board>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=100))]
    rounds: u8,
}

struct Variant {
    root: PathBuf,
    elf: PathBuf,
}

fn prepare(args: &Inputs, suite: &Suite, relative: &Path, root: &Path) -> Result<Variant> {
    let Run::Qemu {
        kernel,
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

    for path in std::iter::once(relative.to_path_buf()).chain(imports.iter().map(PathBuf::from)) {
        let destination = provider.join("server/tests").join(&path);
        for file in composition::hashes(&owner.join(&path))?.keys() {
            let file_path = path.join(file);
            composition::copy(&file_path.to_string_lossy(), &destination.join(file))?;
        }
    }

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
    let mut inputs = serde_json::json!({
        "source": source,
        "suite": suite.id,
        "profile": args.profile,
        "tools": tools(&qemu, &firmware)?,
        "suite_sha256": composition::hashes(&owner.join(relative))?,
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
    })
}

fn boot(args: &Inputs, suite: &Suite, variant: &Variant, hart: u8, round: u8) -> Result<bool> {
    let Run::Qemu {
        expects, distinct, ..
    } = &suite.run
    else {
        return Err("non-QEMU suite cannot boot".into());
    };
    let evidence = variant.root.join(format!("harts-{hart}-round-{round}"));
    fresh(&evidence)?;
    let firmware = Path::new(&std::env::var("QEMU_OPENSBI")?).canonicalize()?;
    let qemu = std::env::var("QEMU_RISCV64").unwrap_or("qemu-system-riscv64".into());
    let mut command = Command::new(qemu);
    command
        .args([
            "-accel",
            "tcg,thread=multi",
            "-machine",
            "virt",
            "-cpu",
            "rv64",
            "-smp",
        ])
        .arg(hart.to_string())
        .args(["-m", "128M", "-monitor", "none", "-nic", "none", "-bios"])
        .arg(firmware)
        .arg("-kernel")
        .arg(&variant.elf);
    let (mut result, shutdown) = qmp::run(command, &evidence, Duration::from_secs(120))?;
    let log = fs::read(evidence.join("serial.log"))?;
    let details = match std::str::from_utf8(&log) {
        Ok(log) => verify::check(log, expects, distinct.as_ref(), hart)?,
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
        "{} {} {} harts={hart} round={round}: {}",
        verdict.to_ascii_uppercase(),
        suite.id,
        args.profile,
        evidence.display()
    );
    Ok(result.passed)
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
    command.args(["+nightly", "test", "--locked"]);
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
    let result = process::run(
        &mut command,
        &args.output.join("cargo"),
        Duration::from_secs(900),
    )?;
    json(&args.output.join("result.json"), &result)?;
    require(result.success(), "Cargo test suite failed")
}

pub fn run(args: Inputs) -> Result<()> {
    let (suite, relative) = suite::read(&args.suite)?;
    require(
        suite.profiles.contains(&args.profile),
        "profile not supported by this test project",
    )?;
    if let Some(board) = args.board {
        require(
            matches!(&suite.run, Run::BoardBoot { boards } if boards.contains(&board)),
            "board not supported by this suite",
        )?;
    }
    let mut args = args;
    args.kernel = args.kernel.canonicalize()?;
    fresh(&args.output)?;
    args.output = args.output.canonicalize()?;
    match &suite.run {
        Run::BoardBoot { boards } => {
            require(!boards.is_empty(), "expected boot boards")?;
            let source = revision(&args.kernel)?;
            let provider = args.output.join("provider");
            archive(&args.kernel, &source, &provider)?;
            json(
                &args.output.join("inputs.json"),
                &serde_json::json!({
                    "source": source, "profile": args.profile, "harts": args.harts, "rustc": output(Command::new("rustc").arg("-Vv"))?
                }),
            )?;
            let script = provider.join("server/tests").join(relative).join("run.sh");
            for board in boards
                .iter()
                .filter(|board| args.board.is_none_or(|want| want == **board))
            {
                for round in 0..args.rounds {
                    let label = format!("{}-{round}", board.name());
                    let mut command = Command::new("bash");
                    command
                        .current_dir(&provider)
                        .arg(&script)
                        .arg(board.name())
                        .arg(&args.profile)
                        .arg(args.output.join(&label))
                        .arg(args.harts.to_string());
                    let result = process::run(
                        &mut command,
                        &args.output.join(format!("{label}-runner")),
                        Duration::from_secs(600),
                    )?;
                    require(
                        result.success(),
                        format!("{label} failed; see {}", args.output.display()),
                    )?;
                }
            }
            Ok(())
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
            let mut passed = true;
            for round in 0..args.rounds {
                passed &= boot(&args, &suite, &variant, args.harts, round)?;
            }
            require(
                passed,
                format!("scenario failed; evidence: {}", args.output.display()),
            )
        }
    }
}
