use super::capture;

#[test]
fn qmp_records_one_shutdown_after_negotiating_and_starting_the_guest() {
    let startup = concat!(
        "{\"QMP\":{}}\r\n",
        "{\"return\":{},\"id\":\"capabilities\"}\n",
        "{\"event\":\"RESUME\"}\n",
        "{\"return\":{},\"id\":\"start\"}\n",
    );
    let event = r#"{"event":"SHUTDOWN","data":{"guest":true,"reason":"guest-shutdown"}}"#;
    let input = format!("{startup}{event}\n");
    let mut commands = Vec::new();
    let mut transcript = Vec::new();
    let receipt = capture(
        input.as_bytes(),
        &mut commands,
        &mut transcript,
        None::<fn() -> std::io::Result<()>>,
    )
    .unwrap();
    assert!(receipt.guest);
    assert_eq!(receipt.reason, "guest-shutdown");
    assert_eq!(transcript, input.as_bytes());
    let commands: Vec<serde_json::Value> = String::from_utf8(commands)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["execute"], "qmp_capabilities");
    assert_eq!(commands[1]["execute"], "cont");

    for incomplete in [
        startup.to_string(),
        format!("{startup}{event}"),
        format!("{input}{event}\n"),
        format!("{input}invalid\n"),
        format!("{input}{{}}\n"),
        input.replace("\"id\":\"start\"", "\"id\":\"other\""),
    ] {
        assert!(
            capture(
                incomplete.as_bytes(),
                Vec::new(),
                Vec::new(),
                None::<fn() -> std::io::Result<()>>,
            )
            .is_err()
        );
    }
}
#[test]
fn qmp_can_observe_execution_owned_by_an_external_controller() {
    let greeting = concat!(
        "{\"QMP\":{}}\n",
        "{\"return\":{},\"id\":\"capabilities\"}\n",
    );
    let resumed = "{\"event\":\"RESUME\"}\n";
    let stopped = "{\"event\":\"STOP\"}\n";
    let shutdown = concat!(
        "{\"event\":\"SHUTDOWN\",",
        "\"data\":{\"guest\":true,\"reason\":\"guest-shutdown\"}}\n",
    );
    let input = format!("{greeting}{resumed}{stopped}{resumed}{shutdown}");
    let mut commands = Vec::new();
    let mut invoked = false;
    let result = capture(
        input.as_bytes(),
        &mut commands,
        Vec::new(),
        Some(|| {
            invoked = true;
            Ok(())
        }),
    )
    .unwrap();
    assert!(invoked && result.guest);
    assert_eq!(String::from_utf8(commands).unwrap().lines().count(), 1);

    let without_resume = format!("{greeting}{shutdown}");
    assert!(
        capture(
            without_resume.as_bytes(),
            Vec::new(),
            Vec::new(),
            Some(|| Ok(())),
        )
        .is_err()
    );
    assert!(
        capture(
            input.as_bytes(),
            Vec::new(),
            Vec::new(),
            Some(|| Err(std::io::Error::other("controller failed"))),
        )
        .is_err()
    );
}
