//! Bounded decoding of the guest's measurement transport.
use crate::{Result, require};
use serde::Serialize;
use std::str::SplitWhitespace;

const SERIES_LIMIT: usize = 16;
const SAMPLE_LIMIT: usize = 64;
const PARTICIPANT_LIMIT: usize = 64;

#[derive(Debug, Serialize)]
pub(super) struct Measurement {
    pub(super) environment: &'static str,
    #[serde(rename = "hart")]
    pub(super) processor: u64,
    pub(super) frequency_hz: u64,
    pub(super) series_count: usize,
    pub(super) series: Vec<Series>,
}

#[derive(Debug, Serialize)]
pub(super) struct Series {
    pub(super) name: String,
    pub(super) participants: usize,
    pub(super) warmup_iterations: u64,
    pub(super) iterations: u64,
    pub(super) sample_count: usize,
    pub(super) elapsed_ns: Vec<u64>,
}

impl Series {
    fn complete(&self) -> bool {
        self.elapsed_ns.len() == self.sample_count
    }
}

fn number(fields: &mut SplitWhitespace<'_>) -> Result<u64> {
    Ok(fields.next().ok_or("missing benchmark field")?.parse()?)
}

pub(super) fn decode(log: &str) -> Result<Measurement> {
    let mut measurement: Option<Measurement> = None;
    for raw in log.split_inclusive('\n') {
        if !raw.starts_with("LIMEN_BENCH") {
            continue;
        }
        require(raw.ends_with('\n'), "truncated benchmark record")?;
        let mut fields = raw.split_whitespace();
        match fields.next() {
            Some("LIMEN_BENCH_V2") => {
                require(measurement.is_none(), "duplicate benchmark header")?;
                let processor = number(&mut fields)?;
                let frequency_hz = number(&mut fields)?;
                let series_count = usize::try_from(number(&mut fields)?)?;
                require(frequency_hz != 0, "invalid counter frequency")?;
                require(
                    (1..=SERIES_LIMIT).contains(&series_count),
                    "invalid series count",
                )?;
                measurement = Some(Measurement {
                    environment: "qemu",
                    processor,
                    frequency_hz,
                    series_count,
                    series: Vec::with_capacity(series_count),
                });
            }
            Some("LIMEN_BENCH_SERIES") => {
                let report = measurement.as_mut().ok_or("series before header")?;
                require(
                    report.series.len() < report.series_count
                        && report.series.last().is_none_or(Series::complete),
                    "unexpected or incomplete series",
                )?;
                let name = fields.next().ok_or("missing benchmark name")?;
                require(
                    !name.is_empty()
                        && name.len() <= 64
                        && name.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        }),
                    "invalid benchmark name",
                )?;
                require(
                    !report.series.iter().any(|series| series.name == name),
                    "duplicate series name",
                )?;
                let participants = usize::try_from(number(&mut fields)?)?;
                let warmup_iterations = number(&mut fields)?;
                let iterations = number(&mut fields)?;
                let sample_count = usize::try_from(number(&mut fields)?)?;
                require(
                    (1..=PARTICIPANT_LIMIT).contains(&participants),
                    "invalid participant count",
                )?;
                require(iterations != 0, "invalid measurement scale")?;
                require(
                    (1..=SAMPLE_LIMIT).contains(&sample_count),
                    "invalid sample count",
                )?;
                report.series.push(Series {
                    name: name.into(),
                    participants,
                    warmup_iterations,
                    iterations,
                    sample_count,
                    elapsed_ns: Vec::with_capacity(sample_count),
                });
            }
            Some("LIMEN_BENCH_SAMPLE") => {
                let series = measurement
                    .as_mut()
                    .and_then(|report| report.series.last_mut())
                    .ok_or("sample before series")?;
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
        measurement.series.len() == measurement.series_count
            && measurement.series.iter().all(Series::complete),
        "incomplete benchmark measurements",
    )?;
    Ok(measurement)
}

#[cfg(test)]
#[path = "../tests/bench/measurement.rs"]
mod tests;
