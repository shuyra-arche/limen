//! Collect benchmark measurements after the ordinary guest completion contract.
use crate::{Result, evidence, native, require};
use serde::Serialize;
use std::{fs, path::Path, str::SplitWhitespace};

const SAMPLE_LIMIT: usize = 64;

#[derive(Debug, Serialize)]
struct Measurement {
    environment: &'static str,
    name: String,
    hart: u64,
    frequency_hz: u64,
    iterations: u64,
    sample_count: usize,
    elapsed_ns: Vec<u64>,
}

fn number(fields: &mut SplitWhitespace<'_>) -> Result<u64> {
    Ok(fields.next().ok_or("missing benchmark field")?.parse()?)
}

fn decode(log: &str) -> Result<Measurement> {
    let mut measurement: Option<Measurement> = None;
    for raw in log.split_inclusive('\n') {
        if !raw.starts_with("LIMEN_BENCH") {
            continue;
        }
        require(raw.ends_with('\n'), "truncated benchmark record")?;
        let mut fields = raw.split_whitespace();
        match fields.next() {
            Some("LIMEN_BENCH_V1") => {
                require(measurement.is_none(), "duplicate benchmark header")?;
                let name = fields.next().ok_or("missing benchmark name")?;
                require(
                    !name.is_empty()
                        && name.len() <= 64
                        && name.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        }),
                    "invalid benchmark name",
                )?;
                let hart = number(&mut fields)?;
                let frequency_hz = number(&mut fields)?;
                let iterations = number(&mut fields)?;
                let sample_count = usize::try_from(number(&mut fields)?)?;
                let valid_scale = frequency_hz != 0 && iterations != 0;
                require(valid_scale, "invalid measurement scale")?;
                require(
                    (1..=SAMPLE_LIMIT).contains(&sample_count),
                    "invalid sample count",
                )?;
                measurement = Some(Measurement {
                    environment: "qemu",
                    name: name.into(),
                    hart,
                    frequency_hz,
                    iterations,
                    sample_count,
                    elapsed_ns: Vec::with_capacity(sample_count),
                });
            }
            Some("LIMEN_BENCH_SAMPLE") => {
                let series = measurement.as_mut().ok_or("sample before header")?;
                let index = usize::try_from(number(&mut fields)?)?;
                let elapsed = number(&mut fields)?;
                require(
                    index == series.elapsed_ns.len() && index < series.sample_count,
                    "unexpected sample index",
                )?;
                require(elapsed != 0, "zero-duration sample")?;
                series.elapsed_ns.push(elapsed);
            }
            _ => return Err("unknown benchmark record".into()),
        }
        require(fields.next().is_none(), "unexpected benchmark fields")?;
    }
    let measurement = measurement.ok_or("missing benchmark header")?;
    require(
        measurement.elapsed_ns.len() == measurement.sample_count,
        "incomplete benchmark measurements",
    )?;
    Ok(measurement)
}

pub fn run(image: &Path, evidence_root: &Path) -> Result<()> {
    let directory = native::run(image, false, Some(evidence_root))?;
    let result = (|| {
        let log = fs::read_to_string(directory.join("serial.log"))?;
        let measurement = decode(&log)?;
        evidence::json(&directory.join("measurement.json"), &measurement)
    })();
    if let Err(error) = &result {
        fs::write(directory.join("measurement-error.txt"), error.to_string())?;
    }
    result
}

#[cfg(test)]
#[path = "tests/bench.rs"]
mod tests;
