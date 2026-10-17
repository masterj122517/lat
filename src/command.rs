use std::{
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Captures small native-tool responses without letting an OS integration hang
/// the collector. A child blocked on a full output pipe is also timed out.
pub(crate) fn output(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("could not read {program} response: {error}"))?;
                if output.status.success() {
                    return Ok(output);
                }
                let diagnostic = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "{program} exited with {}: {}",
                    output.status,
                    diagnostic.trim()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for {program}: {error}"));
            }
            Ok(None) => {}
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} timed out after {} ms",
                timeout.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(20).min(timeout.saturating_sub(started.elapsed())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_native_failures_and_bounds_stalled_commands() {
        let error = output(
            Command::new("/bin/sh").args(["-c", "printf 'permission denied' >&2; exit 7"]),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.contains("permission denied") && error.contains('7'));
        let error = output(
            Command::new("/bin/sleep").arg("2"),
            Duration::from_millis(30),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
    }
}
