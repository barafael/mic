//! The `relay` phase: frames pass through the box's relay between a stock moq publisher and
//! subscriber (moq-tokio's `clock` example), over QUIC, with the box's CA. The baseline for every
//! later failure: if this fails, the box is broken, not `mic`.

use crate::error::{Error, Result};
use crate::podman::{Pod, TestBox};
use crate::{process, ready};

use super::Checks;

/// The relay as the pods reach it: its pod's name, which its certificate names too.
const RELAY: &str = "https://mic-testbox-relay:4443/";
/// The box's CA (testbox/moq/keys.sh).
const CA: &str = "/keys/ca.pem";
/// How long the subscriber listens: the clock publishes a frame a second.
const LISTEN_SECS: u64 = 5;
/// How long the subscriber without the CA listens: the handshake fails at once.
const REFUSED_SECS: u64 = 3;
/// How many frames it must hear in that time. The first may take a moment to come.
const FRAMES: usize = 3;

pub fn relay(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    checks.require("the relay serves", || ready::relay(the_box))?;
    // At once: each listens for seconds, and they don't share a broadcast.
    let (frames, refusal) = std::thread::scope(|scope| {
        let frames = scope.spawn(|| frames_pass(the_box));
        let refusal = scope.spawn(|| refused(the_box));
        (joined(frames.join()), joined(refusal.join()))
    });
    checks.record("frames pass through the relay", || frames);
    checks.record("a client without the box's CA is refused", || refusal);
    Ok(())
}

/// A check run on a thread of its own: a panic there fails it.
fn joined(outcome: std::thread::Result<Result<()>>) -> Result<()> {
    outcome.unwrap_or_else(|_| Err(Error::Failed("its thread panicked".to_owned())))
}

fn tools() -> String {
    Pod::Tools.container("tools")
}

/// `clock` publishes; another `clock` subscribes through the relay and must print frames.
fn frames_pass(the_box: &TestBox) -> Result<()> {
    let broadcast = process::unique("relay-check")?;
    let publish =
        format!("clock --connect {RELAY} --connect-tls-root {CA} --broadcast {broadcast} publish");
    // In the background, for longer than the subscriber listens; it ends on its own.
    let started = the_box.exec(
        &tools(),
        &[
            "bash",
            "-c",
            &format!("timeout {} {publish} >/dev/null 2>&1 &", LISTEN_SECS + 10),
        ],
    )?;
    if !started.status.success() {
        return Err(Error::Failed("the publisher didn't start".to_owned()));
    }
    let heard = subscribe(
        the_box,
        &broadcast,
        &["--connect-tls-root", CA],
        LISTEN_SECS,
    )?;
    let frames: Vec<&str> = heard.lines().filter(|line| is_clock_frame(line)).collect();
    let mut distinct = frames.clone();
    distinct.dedup();
    if distinct.len() >= FRAMES {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{} distinct frames in {LISTEN_SECS}s, wanted {FRAMES}: {heard:?}",
            distinct.len()
        )))
    }
}

/// Without the CA, the subscriber must not connect, so it hears nothing: the relay's certificate
/// is checked, not taken on faith.
fn refused(the_box: &TestBox) -> Result<()> {
    let broadcast = process::unique("relay-check")?;
    let heard = subscribe(the_box, &broadcast, &[], REFUSED_SECS)?;
    if heard.lines().any(is_clock_frame) {
        Err(Error::Failed(format!("it heard frames: {heard:?}")))
    } else {
        Ok(())
    }
}

/// What a subscriber to `broadcast` prints in `seconds`, connecting with `tls`.
fn subscribe(the_box: &TestBox, broadcast: &str, tls: &[&str], seconds: u64) -> Result<String> {
    let listen = seconds.to_string();
    let mut argv = vec!["timeout", &listen, "clock", "--connect", RELAY];
    argv.extend_from_slice(tls);
    argv.extend_from_slice(&["--broadcast", broadcast, "subscribe"]);
    let heard = the_box.exec(&tools(), &argv)?;
    Ok(String::from_utf8_lossy(&heard.stdout).into_owned())
}

/// A line the clock prints for a frame: `YYYY-MM-DD HH:MM:SS`.
fn is_clock_frame(line: &str) -> bool {
    let shape = "dddd-dd-dd dd:dd:dd";
    line.len() == shape.len()
        && line.chars().zip(shape.chars()).all(|(c, s)| match s {
            'd' => c.is_ascii_digit(),
            _ => c == s,
        })
}

#[cfg(test)]
mod tests {
    use super::is_clock_frame;

    #[test]
    fn a_clock_frame_is_a_time_of_day() {
        assert!(is_clock_frame("2026-10-26 21:07:05"));
        assert!(!is_clock_frame("2026-10-26 21:07:"));
        assert!(!is_clock_frame("connecting to server"));
    }
}
