//! When the box is ready, said by the box itself rather than waited for with fixed sleeps: the
//! relay answers, and every client says it's up in its log. (With the harness, from M2 on, a
//! client is up once it answers `Ping`.)

use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::podman::{CLIENTS, Pod, TestBox};
use crate::view;

/// How long the relay and each client may take after their pods were played.
pub const START: Duration = Duration::from_secs(30);
/// What a client logs once it's up: the probe window's first frame (testbox/probe, `PRESENTED`).
pub const CLIENT_UP: &str = "probe: presented";

/// Waits until the relay answers and every client is up.
pub fn all(the_box: &TestBox) -> Result<()> {
    relay(the_box)?;
    for client in CLIENTS {
        client_up(the_box, client)?;
    }
    Ok(())
}

/// The relay answers HTTP on TCP 4443 (`/announced`, what is announced), which it serves beside
/// QUIC, as asked from inside the box.
pub fn relay(the_box: &TestBox) -> Result<()> {
    let deadline = Instant::now() + START;
    loop {
        let asked = the_box.exec(
            &Pod::Tools.container("tools"),
            &[
                "curl",
                "--silent",
                "--fail",
                "--max-time",
                "2",
                "http://mic-testbox-relay:4443/announced",
            ],
        )?;
        if asked.status.success() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "the relay didn't answer within {}s: {}",
                START.as_secs(),
                String::from_utf8_lossy(&asked.stderr).trim()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `client` has logged [`CLIENT_UP`]; returns its log.
pub fn client_up(the_box: &TestBox, client: &'static str) -> Result<String> {
    let container = view::container(client);
    let deadline = Instant::now() + START;
    loop {
        let log = the_box.log(&container)?;
        if log.lines().any(|line| line == CLIENT_UP) {
            return Ok(log);
        }
        if Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "{client} wasn't up within {}s: {:?}",
                START.as_secs(),
                log.lines().last().unwrap_or_default()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
