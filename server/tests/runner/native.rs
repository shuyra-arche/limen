//! Interpret the ordinary guest completion contract.
use crate::{Result, evidence, machine, platform::Purpose, process, require};
use serde::Serialize;
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Verdict {
    Pass,
    Fail,
    Timeout,
    Inconclusive,
}

#[derive(Debug, Serialize)]
struct Report {
    verdict: Verdict,
    reason: String,
}

impl Report {
    fn new(verdict: Verdict, reason: impl Into<String>) -> Self {
        Self {
            verdict,
            reason: reason.into(),
        }
    }
}

fn judge(outcome: &process::Outcome, observation_complete: bool) -> Report {
    use process::Termination;
    if outcome.termination == Termination::Timeout {
        return Report::new(Verdict::Timeout, "execution deadline reached");
    }
    if !outcome.clean() || !observation_complete {
        return Report::new(
            Verdict::Inconclusive,
            "incomplete or conflicting execution and retirement evidence",
        );
    }
    match outcome.termination {
        Termination::Exited(97) => Report::new(Verdict::Pass, "explicit guest completion"),
        Termination::Exited(99) => Report::new(Verdict::Fail, "explicit guest failure"),
        Termination::Exited(_) | Termination::Signaled(_) => Report::new(
            Verdict::Fail,
            "guest terminated without explicit completion",
        ),
        _ => Report::new(Verdict::Inconclusive, "unknown termination"),
    }
}

fn execute(image: &Path, purpose: Purpose, directory: &Path, observe: bool) -> Result<Report> {
    let mut command = machine::prepare(image, purpose, directory, observe)?;
    let timeout = Duration::from_secs(45);
    if observe {
        let (outcome, shutdown) = crate::qmp::run_isolated(command, directory, timeout, None)?;
        let complete = shutdown.is_ok_and(|shutdown| {
            shutdown.guest
                && (outcome.termination != process::Termination::Exited(97)
                    || shutdown.reason == "guest-shutdown")
        });
        Ok(judge(&outcome, complete))
    } else {
        let outcome = process::run(&mut command, directory, timeout)?;
        Ok(judge(&outcome, true))
    }
}

pub fn run(
    image: &Path,
    purpose: Purpose,
    observe: bool,
    evidence_root: Option<&Path>,
) -> Result<PathBuf> {
    let image = image.canonicalize()?;
    let evidence = match evidence_root {
        Some(root) => root.join(image.file_name().ok_or("missing executable name")?),
        None => PathBuf::from(env::var_os("LIMEN_TEST_EVIDENCE").ok_or("missing evidence path")?),
    };
    evidence::fresh(&evidence)?;
    let directory = evidence.canonicalize()?;
    let path = directory.join("result.json");
    // An interrupted attempt cannot retain a successful verdict from an earlier run.
    evidence::json(
        &path,
        &Report::new(Verdict::Inconclusive, "execution incomplete"),
    )?;
    let report = execute(&image, purpose, &directory, observe)
        .unwrap_or_else(|error| Report::new(Verdict::Inconclusive, error.to_string()));
    evidence::json(&path, &report)?;
    require(
        report.verdict == Verdict::Pass,
        format!("native guest verdict: {report:?}"),
    )?;
    Ok(directory)
}

#[cfg(test)]
#[path = "tests/native.rs"]
mod tests;
