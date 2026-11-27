//! Execute the benchmark contract and retain its measurements.
use crate::{Result, evidence, native, platform::Purpose};
use std::{env, fs, path::Path};

mod artifact;
mod comparison;
mod measurement;
mod selection;
mod statistics;
mod workload;

pub use comparison::compare;
pub use selection::run_set;

pub fn run(image: &Path, evidence_root: &Path) -> Result<()> {
    run_in(image, evidence_root, &env::current_dir()?)
}

fn run_in(image: &Path, evidence_root: &Path, working: &Path) -> Result<()> {
    let directory = native::run(image, Purpose::Bench, false, Some(evidence_root))?;
    let result = (|| {
        let log = fs::read_to_string(directory.join("serial.log"))?;
        let measurement = measurement::decode(&log)?;
        let workload = workload::capture(image, working)?;
        evidence::json(&directory.join("workload.json"), &workload)?;
        evidence::json(&directory.join("measurement.json"), &measurement)?;
        evidence::json(
            &directory.join("summary.json"),
            &statistics::summarize(&measurement),
        )
    })();
    if let Err(error) = &result {
        fs::write(directory.join("measurement-error.txt"), error.to_string())?;
    }
    result
}
