use kernel_verification::{Result, process};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::Path,
    process::Command,
    thread,
    time::Duration,
};

#[derive(Debug, Deserialize, Serialize)]
pub struct Shutdown {
    pub guest: bool,
    pub reason: String,
}

// A private control socket keeps QMP independent of guest and firmware stdout.
#[cfg(unix)]
pub fn run_isolated(
    mut command: Command,
    directory: &Path,
    timeout: Duration,
) -> Result<(process::Outcome, io::Result<Shutdown>)> {
    use std::{
        fs,
        os::unix::net::UnixListener,
        process::Stdio,
        sync::atomic::{AtomicBool, Ordering},
    };

    let socket = directory.join("qmp.sock");
    let transcript = File::create(directory.join("qmp.jsonl"))?;
    let stdout = File::create(directory.join("stdout.log"))?;
    let listener = UnixListener::bind(&socket)?;
    let result = (|| {
        listener.set_nonblocking(true)?;
        command
            .args(["-S", "-qmp"])
            .arg(format!("unix:{},server=off", socket.display()));
        let finished = AtomicBool::new(false);
        thread::scope(|scope| {
            let monitor = scope.spawn(|| {
                let (stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            if finished.load(Ordering::Acquire) {
                                return Err(io::Error::other("QMP did not connect"));
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => return Err(error),
                    }
                };
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(timeout))?;
                stream.set_write_timeout(Some(timeout))?;
                let commands = stream.try_clone()?;
                capture(BufReader::new(stream), commands, transcript)
            });
            let outcome = process::run_with_stdio(
                &mut command,
                directory,
                timeout,
                Stdio::null(),
                stdout.into(),
            );
            finished.store(true, Ordering::Release);
            let shutdown = monitor.join().map_err(|_| "QMP reader panicked")?;
            Ok((outcome?, shutdown))
        })
    })();
    fs::remove_file(socket)?;
    result
}

#[cfg(not(unix))]
pub fn run_isolated(
    _command: Command,
    _directory: &Path,
    _timeout: Duration,
) -> Result<(process::Outcome, io::Result<Shutdown>)> {
    Err("isolated QMP observation requires a Unix host".into())
}

pub(super) fn capture(
    mut input: impl BufRead,
    mut commands: impl Write,
    mut transcript: impl Write,
) -> io::Result<Shutdown> {
    let mut bytes = Vec::new();
    let mut next = || -> io::Result<Option<Value>> {
        bytes.clear();
        if input.read_until(b'\n', &mut bytes)? == 0 {
            return Ok(None);
        }
        transcript.write_all(&bytes)?;
        if !bytes.ends_with(b"\n") {
            return Err(io::Error::other("truncated QMP record"));
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    };
    if !next()?.is_some_and(|message| message["QMP"].is_object()) {
        return Err(io::Error::other("missing QMP greeting"));
    }
    writeln!(
        commands,
        r#"{{"execute":"qmp_capabilities","id":"capabilities"}}"#
    )?;
    commands.flush()?;
    let mut started = false;
    let mut acknowledged = false;
    let mut shutdown = None;
    while let Some(message) = next()? {
        if message.get("error").is_some() {
            return Err(io::Error::other(format!("QMP command failed: {message}")));
        }
        match (message["id"].as_str(), message["event"].as_str()) {
            (Some("capabilities"), _) if !started && message["return"].is_object() => {
                // The VM is paused until its event consumer has negotiated QMP.
                writeln!(commands, r#"{{"execute":"cont","id":"start"}}"#)?;
                commands.flush()?;
                started = true;
            }
            (Some("start"), _) if started && !acknowledged && message["return"].is_object() => {
                acknowledged = true;
            }
            (Some(_), _) => return Err(io::Error::other("unexpected QMP reply")),
            (_, Some("SHUTDOWN")) => {
                if !started || shutdown.is_some() {
                    return Err(io::Error::other("unexpected or duplicate QMP shutdown"));
                }
                shutdown = Some(serde_json::from_value(message["data"].clone())?);
            }
            (None, Some(_)) => {}
            _ => return Err(io::Error::other("invalid QMP message")),
        }
    }
    if !acknowledged {
        return Err(io::Error::other("QMP did not acknowledge guest execution"));
    }
    shutdown.ok_or_else(|| io::Error::other("missing QMP shutdown"))
}
