//! Boot-fault observation contracts shared by the Host observers.

#[cfg(all(unix, limen_guest = "riscv64"))]
mod riscv64;

fn observation<const N: usize>(log: &str, name: &str) -> [u64; N] {
    let mut lines = log.lines().filter_map(|line| {
        let (tag, fields) = line.split_once(' ')?;
        (tag == name).then_some(fields)
    });
    let values: Vec<_> = lines
        .next()
        .unwrap_or_else(|| panic!("missing {name}: {log}"))
        .split_whitespace()
        .map(|field| u64::from_str_radix(field, 16).unwrap())
        .collect();
    assert!(lines.next().is_none(), "duplicate {name}");
    values.try_into().expect("complete register observation")
}

#[test]
fn register_observations_are_exact_and_complete() {
    let log = "debugger output\nLIMEN_CLEAR 80200040 80208000 80200100\n";
    assert_eq!(
        observation::<3>(log, "LIMEN_CLEAR"),
        [0x8020_0040, 0x8020_8000, 0x8020_0100]
    );
}

fn diagnostic(logs: &[&str], cause: u64, pc: u64, value: u64, stack: u64, status: u64) {
    let expected = format!(
        "FAIL Limen boot trap: cause={cause:#x} pc={pc:#x} value={value:#x} stack={stack:#x} status={status:#x}"
    );
    let mut failures = Vec::new();
    // The actual firmware may deliver its console output through any of
    // these recorded channels. Require one complete diagnostic across them.
    for log in logs {
        assert!(!log.contains("PASS boot-handoff"));
        for line in log.split_inclusive('\n') {
            let record = line.trim_end_matches(['\r', '\n']);
            if record == "FAIL" || record.starts_with("FAIL ") {
                assert!(line.ends_with('\n'), "truncated diagnostic: {line}");
                failures.push(record);
            }
        }
    }
    assert_eq!(failures, [expected.as_str()], "{logs:?}");
}

#[test]
fn terminal_diagnostic_is_unique_across_the_recorded_channels() {
    let record = "FAIL Limen boot trap: cause=0x2 pc=0x100 value=0x0 stack=0x200 status=0x300\r\n";
    for index in 0..3 {
        let mut logs = [""; 3];
        logs[index] = record;
        diagnostic(&logs, 2, 0x100, 0, 0x200, 0x300);
    }
    for logs in [
        [record, record, ""],
        ["", "", ""],
        [record.trim_end(), "", ""],
        [record, "PASS boot-handoff\n", ""],
        [record, "FAIL unrelated\n", ""],
    ] {
        assert!(std::panic::catch_unwind(|| diagnostic(&logs, 2, 0x100, 0, 0x200, 0x300)).is_err());
    }
}

#[test]
fn terminal_diagnostic_must_match_the_observed_registers() {
    let record = "FAIL Limen boot trap: cause=0x2 pc=0x100 value=0x0 stack=0x200 status=0x300\n";
    assert!(std::panic::catch_unwind(|| diagnostic(&[record], 2, 0x102, 0, 0x200, 0x300)).is_err());
}
