use kernel_verification::{Result, require};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    num::NonZeroU8,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub id: String,
    pub profiles: Vec<String>,
    pub sources: Vec<String>,
    pub run: Run,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Run {
    Qemu {
        kernel: String,
        #[serde(default)]
        hardware: QemuHardware,
        #[serde(default)]
        harts: Option<NonZeroU8>,
        #[serde(default)]
        imports: Vec<String>,
        #[serde(default)]
        features: Vec<String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        expects: Vec<String>,
        distinct: Option<Distinct>,
    },
    BoardBoot {
        boards: Vec<Board>,
        #[serde(default)]
        harts: Option<NonZeroU8>,
        #[serde(default)]
        board_sources: BTreeMap<String, Vec<String>>,
        #[serde(default)]
        imports: Vec<String>,
        launcher: Option<PathBuf>,
        #[serde(default)]
        launch: BTreeMap<String, BoardLaunch>,
    },
    CargoTest {
        package: Option<String>,
        manifest: Option<String>,
        test: String,
        ignored: bool,
    },
}

#[derive(Clone, Debug, Default, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct QemuHardware {
    machine: Option<QemuComponent>,
    cpu: Option<QemuComponent>,
    #[serde(default)]
    devices: Vec<QemuComponent>,
}

impl QemuHardware {
    pub(crate) fn valid(&self) -> bool {
        self.machine
            .iter()
            .chain(&self.cpu)
            .chain(&self.devices)
            .all(QemuComponent::valid)
    }

    pub fn arguments(&self) -> Vec<String> {
        let mut args = vec![
            "-machine".into(),
            self.machine
                .as_ref()
                .map_or_else(|| "virt".into(), QemuComponent::argument),
            "-cpu".into(),
            self.cpu
                .as_ref()
                .map_or_else(|| "rv64".into(), QemuComponent::argument),
        ];
        for device in &self.devices {
            args.extend(["-device".into(), device.argument()]);
        }
        args
    }
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct QemuComponent {
    name: String,
    #[serde(default)]
    options: BTreeMap<String, String>,
}

impl QemuComponent {
    fn valid(&self) -> bool {
        name(&self.name) && options(&self.options)
    }

    fn argument(&self) -> String {
        let mut argument = self.name.clone();
        for (key, value) in &self.options {
            argument.push(',');
            argument.push_str(key);
            argument.push('=');
            argument.push_str(value);
        }
        argument
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardLaunch {
    kernel: Option<String>,
    #[serde(default)]
    machine_options: BTreeMap<String, String>,
    #[serde(default)]
    devices: Vec<QemuComponent>,
    #[serde(default)]
    secondary_serial: bool,
    expects: Vec<String>,
    #[serde(default)]
    forbids: Vec<String>,
}

impl BoardLaunch {
    fn valid(&self) -> bool {
        self.kernel.as_ref().is_none_or(|value| name(value))
            && options(&self.machine_options)
            && self.devices.iter().all(QemuComponent::valid)
            && assertions(&self.expects)
            && self.forbids.iter().all(|line| assertion(line))
    }
}

fn name(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

fn options(options: &BTreeMap<String, String>) -> bool {
    options.iter().all(|(key, value)| {
        name(key)
            && !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:+".contains(&byte))
    })
}

fn assertion(line: &str) -> bool {
    !line.trim().is_empty() && !line.contains(['\n', '\r', '\0'])
}

fn assertions(lines: &[String]) -> bool {
    !lines.is_empty() && lines.iter().all(|line| assertion(line))
}

fn relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Board {
    Q35,
    Virt,
}

impl Board {
    pub fn name(self) -> &'static str {
        match self {
            Self::Q35 => "q35",
            Self::Virt => "virt",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Distinct {
    pub pattern: String,
    pub count: usize,
}

pub fn read(path: &Path) -> Result<(Suite, PathBuf)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing test root")?
        .canonicalize()?;
    let target = root.join("target").canonicalize()?;
    let directory = path.canonicalize()?;
    require(
        directory.starts_with(&target),
        "test project must be inside server/tests/target",
    )?;
    let relative = directory.strip_prefix(&root)?.to_owned();
    let suite: Suite = serde_json::from_slice(&fs::read(directory.join("suite.json"))?)?;
    require(
        !suite.id.is_empty()
            && suite
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
        "invalid test project id",
    )?;
    require(
        !suite.profiles.is_empty()
            && suite
                .profiles
                .iter()
                .all(|profile| profile == "debug" || profile == "release"),
        "invalid test profiles",
    )?;
    require(
        !suite.sources.is_empty(),
        "test project has no source owners",
    )?;
    match &suite.run {
        Run::Qemu {
            kernel,
            hardware,
            imports,
            features,
            expects,
            ..
        } => {
            require(
                name(kernel) && features.iter().all(|feature| name(feature)),
                "invalid QEMU guest binary or feature declaration",
            )?;
            require(hardware.valid(), "invalid QEMU hardware declaration")?;
            require(
                assertions(expects),
                "QEMU suite requires nonempty result assertions",
            )?;
            require(
                imports
                    .iter()
                    .all(|import| relative_path(Path::new(import))),
                "test imports must stay within the test tree",
            )?;
        }
        Run::BoardBoot {
            boards,
            board_sources,
            imports,
            launcher,
            launch,
            ..
        } => {
            let names: BTreeSet<_> = boards.iter().map(|board| board.name()).collect();
            require(
                !names.is_empty()
                    && names.len() == boards.len()
                    && board_sources
                        .keys()
                        .all(|board| names.contains(board.as_str())),
                "invalid boot boards or board source owners",
            )?;
            require(
                imports
                    .iter()
                    .all(|import| relative_path(Path::new(import))),
                "test imports must stay within the test tree",
            )?;
            match launcher {
                Some(launcher) => {
                    require(
                        relative_path(launcher),
                        "launcher must be relative to the target test tree",
                    )?;
                    let script = target.join(launcher).canonicalize()?;
                    require(
                        script.starts_with(&target) && script.is_file(),
                        "launcher must be in the target test tree",
                    )?;
                    let manifest = directory.join("Cargo.toml").canonicalize()?;
                    require(
                        manifest.starts_with(&directory) && manifest.is_file(),
                        "board guest manifest must be owned by the scenario",
                    )?;
                    require(
                        launch.keys().map(String::as_str).collect::<BTreeSet<_>>() == names
                            && launch.iter().all(|(board, declaration)| {
                                declaration.valid()
                                    && (!declaration.secondary_serial || board == "virt")
                            }),
                        "invalid board launch declaration",
                    )?;
                }
                None => require(
                    launch.is_empty(),
                    "board launch declaration requires a launcher",
                )?,
            }
        }
        Run::CargoTest {
            package, manifest, ..
        } => {
            require(
                package.is_some() || manifest.is_some(),
                "Cargo test requires a package or local manifest",
            )?;
            if let Some(manifest) = manifest {
                require(
                    relative_path(Path::new(manifest)),
                    "Cargo manifest must be local to the project",
                )?;
            }
        }
    }
    Ok((suite, relative))
}
