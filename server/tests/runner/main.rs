use clap::{Parser, Subcommand};
use kernel_verification::{Result, native, utilities};
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
        } => native::run(&image, observe, evidence_root.as_deref()),
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
