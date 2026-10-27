//! When the box is ready, said by the box itself rather than waited for with fixed sleeps: the
//! relay answers, every display's program says it's up in its log, and every client answers
//! `Ping` on its harness.

use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{CLIENTS, DISPLAYS, Pod, TestBox};
use crate::view;

/// How long the relay and each client may take after their pods were played.
pub const START: Duration = Duration::from_secs(30);
/// What the probe logs once it's up: its window's first frame (testbox/probe, `PRESENTED`).
pub const PROBE_UP: &str = "probe: presented";

/// What `display`'s program logs once it's up: the probe's, or a client's window's first frame.
fn up_line(display: &str) -> &'static str {
    match display {
        "probe" => PROBE_UP,
        _ => mic_harness::SHOWN,
    }
}

/// Waits until the relay answers, and every display and client is up.
pub fn all(the_box: &TestBox) -> Result<()> {
    relay(the_box)?;
    site(the_box)?;
    for display in DISPLAYS {
        display_up(the_box, display)?;
    }
    for client in CLIENTS {
        client_up(the_box, client)?;
    }
    for index in 1..=the_box.crowd() {
        crowd_up(the_box, index)?;
    }
    Ok(())
}

/// The crowd's participant `index` answers `Ping` on its harness.
pub fn crowd_up(the_box: &TestBox, index: u32) -> Result<()> {
    let deadline = Instant::now() + START;
    loop {
        let pinged = Participant::crowd(the_box, index).and_then(|mut member| member.ping());
        match pinged {
            Ok(_) => return Ok(()),
            Err(error) if Instant::now() > deadline => {
                return Err(Error::Failed(format!(
                    "{} didn't answer within {}s: {error}",
                    crate::podman::crowd_name(index),
                    START.as_secs()
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// The relay answers HTTP on TCP 4443 (`/announced`, what is announced), which it serves beside
/// QUIC, as asked from inside the box.
pub fn relay(the_box: &TestBox) -> Result<()> {
    answers(
        the_box,
        "the relay",
        "http://mic-testbox-relay:4443/announced",
    )
}

/// The site serves `version.json`, as asked from inside the box.
pub fn site(the_box: &TestBox) -> Result<()> {
    answers(
        the_box,
        "the site",
        "http://mic-testbox-site:8080/version.json",
    )
}

/// `url` answers, asked from the tools pod, within [`START`].
fn answers(the_box: &TestBox, what: &str, url: &str) -> Result<()> {
    let deadline = Instant::now() + START;
    loop {
        let asked = the_box.exec(
            &Pod::Tools.container("tools"),
            &["curl", "--silent", "--fail", "--max-time", "2", url],
        )?;
        if asked.status.success() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "{what} didn't answer within {}s: {}",
                START.as_secs(),
                String::from_utf8_lossy(&asked.stderr).trim()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `display` has logged that it's up ([`up_line`]); returns its log.
pub fn display_up(the_box: &TestBox, display: &'static str) -> Result<String> {
    let container = view::container(display);
    let deadline = Instant::now() + START;
    loop {
        let log = the_box.log(&container)?;
        if log.lines().any(|line| line == up_line(display)) {
            return Ok(log);
        }
        if Instant::now() > deadline {
            return Err(Error::Failed(format!(
                "{display} wasn't up within {}s: {:?}",
                START.as_secs(),
                log.lines().last().unwrap_or_default()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `client` answers `Ping` on its harness.
pub fn client_up(the_box: &TestBox, client: &'static str) -> Result<()> {
    let deadline = Instant::now() + START;
    loop {
        let pinged = Participant::connect(the_box, client).and_then(|mut client| client.ping());
        match pinged {
            Ok(_) => return Ok(()),
            Err(error) if Instant::now() > deadline => {
                return Err(Error::Failed(format!(
                    "{client} didn't answer within {}s: {error}",
                    START.as_secs()
                )));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
