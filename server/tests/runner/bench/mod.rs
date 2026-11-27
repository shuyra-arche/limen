//! Execute the benchmark contract and retain its measurements.
use crate::{Result, evidence, native};
use std::{fs, path::Path};

mod artifact;
mod comparison;
mod measurement;
mod statistics;
mod workload;

pub use comparison::compare;

pub fn run(image: &Path, evidence_root: &Path) -> Result<()> {
    let directory = native::run(image, false, Some(evidence_root))?;
    let result = (|| {
        let log = fs::read_to_string(directory.join("serial.log"))?;
        let measurement = measurement::decode(&log)?;
        let workload = workload::capture(image)?;
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
