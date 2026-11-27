use clap::{Parser, Subcommand};
use kernel_verification::{Result, bench, native, utilities};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Kernel target verification")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Native {
        #[arg(long)]
        observe: bool,
        #[arg(long)]
        evidence_root: Option<PathBuf>,
        image: PathBuf,
    },
    Bench {
        #[arg(long)]
        evidence_root: PathBuf,
        image: PathBuf,
        // Cargo appends this flag to benchmark executables.
        #[arg(long)]
        bench: bool,
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
        Commands::Native {
            observe,
            evidence_root,
            image,
        } => native::run(&image, observe, evidence_root.as_deref()).map(|_| ()),
        Commands::Bench {
            evidence_root,
            image,
            bench: true,
        } => bench::run(&image, &evidence_root),
        Commands::Bench { .. } => Err("expected Cargo benchmark invocation (--bench)".into()),
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
