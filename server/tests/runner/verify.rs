use crate::suite::Distinct;
use kernel_verification::{Result, process::Outcome};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn check(
    log: &str,
    expects: &[String],
    distinct: Option<&Distinct>,
    harts: u8,
) -> Result<Value> {
    let missing: Vec<_> = expects
        .iter()
        .map(|marker| marker.replace("{harts}", &harts.to_string()))
        .filter(|marker| {
            !log.split_inclusive('\n')
                .filter_map(|line| line.strip_suffix('\n'))
                .any(|line| line.strip_suffix('\r').unwrap_or(line) == marker)
        })
        .collect();
    let mut evidence = json!({});
    let mut valid = true;
    if let Some(rule) = distinct {
        let pattern = Regex::new(&rule.pattern)?;
        let values: Vec<_> = pattern
            .captures_iter(log)
            .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_owned()))
            .collect();
        let unique: BTreeSet<_> = values.iter().collect();
        valid &= values.len() == rule.count && unique.len() == rule.count;
        evidence = json!({"distinct": values});
    }
    let assertion_failed = log.lines().any(|line| {
        line == "FAIL" || line.starts_with("FAIL ") || line.starts_with("[kernel] fatal")
    });
    Ok(json!({
        "passed": valid && missing.is_empty() && !assertion_failed,
        "assertion_failed": assertion_failed,
        "missing": missing,
        "evidence": evidence
    }))
}

pub fn verdict(result: &Outcome, details: &Value) -> &'static str {
    if result.status == "timeout" {
        "timeout"
    } else if details["assertion_failed"] == true || (result.exited() && !result.success()) {
        "fail"
    } else if result.success() && details["passed"] == true {
        "pass"
    } else {
        "inconclusive"
    }
}
