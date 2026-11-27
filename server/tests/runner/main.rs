use clap::{Parser, Subcommand};
use kernel_verification::Result;
use std::path::PathBuf;

mod composition;
mod native;
mod qmp;
mod scenario;
mod suite;
mod utilities;
mod verify;

#[derive(Parser)]
#[command(about = "Kernel target verification")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run(scenario::Inputs),
    Native {
        #[arg(long)]
        observe: bool,
        #[arg(long)]
        evidence_root: Option<PathBuf>,
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=64))]
        harts: u8,
        architecture: String,
        image: PathBuf,
    },
    Observe {
        elf: PathBuf,
        output: PathBuf,
    },
    Symbolize {
        elf: PathBuf,
        serial: PathBuf,
        #[arg(long, default_value = "llvm-addr2line")]
        addr2line: String,
    },
}

fn main() {
    let result: Result<()> = match Cli::parse().command {
        Commands::Run(inputs) => scenario::run(inputs),
        Commands::Native {
            observe,
            evidence_root,
            harts,
            architecture,
            image,
        } => native::run(&architecture, &image, observe, evidence_root.as_deref(), harts),
        Commands::Observe { elf, output } => scenario::observe(elf, output),
        Commands::Symbolize {
            elf,
            serial,
            addr2line,
        } => utilities::symbolize(&elf, &serial, &addr2line),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
