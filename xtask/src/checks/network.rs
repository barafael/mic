//! The `network` phase (PLAN.md, commits 46 and 47): alice's pod's network shaped from its
//! `shaper` sidecar (testbox/pods/client.yaml), which shares the pod's network namespace with
//! NET_ADMIN over it alone.
//!
//! - With 10 % of what she sends lost and 80 ms added to it, bob still hears her tone, louder
//!   than anything else he hears.
//! - With 30 % lost for three seconds, her camera at bob's end recovers once the loss stops, at
//!   its next keyframe.
//! - With UDP blocked, she joins all the same, over the WebSocket fallback, and bob hears her.
//! - With UDP blocked and the fallback opted out of (`--no-websocket`), a join fails, and says
//!   why.
//!
//! What's measured goes into the record (`network.txt`). The shaping is cleared at the start and
//! however the phase ends. It disrupts alice's network, so it runs alone.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::{Participant, names};
use crate::podman::{Pod, TestBox, camera, tone};

use super::Checks;

/// How long a roster, a voice or a camera may take to show.
const PATIENCE: Duration = Duration::from_secs(20);
/// How loud the tone must stay under loss, at the least: below the clean [`crate::harness::LOUD`],
/// as what's lost is concealed, not recovered.
const LOUD_UNDER_LOSS: f64 = 0.15;
/// How many times louder than any other frequency listened at alice's tone must be: dominant.
/// Not a share of the power, which a gap's click spreads across the spectrum: under loss a voice
/// group waits for what's sent again, and the stream underruns, then overflows its 120 ms
/// (docs/observations.md).
const DOMINANT: f64 = 10.0;
/// Frequencies listened at beside the tone: apart from 440 Hz's harmonics and bob's 660.
const OTHERS: [f64; 5] = [300.0, 550.0, 770.0, 1000.0, 1250.0];
/// How many pictures of alice's camera make it recovered.
const RECOVERED: u64 = 10;

pub fn network(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let shaper = Shaper { the_box };
    shaper.clear()?;
    let (mut alice, mut bob) = checks.require("alice and bob answer", || {
        Ok((
            Participant::connect(the_box, "alice")?,
            Participant::connect(the_box, "bob")?,
        ))
    })?;
    let (alices, bobs) = (
        f64::from(tone("alice").ok_or("alice sends no tone")?),
        f64::from(tone("bob").ok_or("bob sends no tone")?),
    );
    let pattern = camera("alice").ok_or("alice sends no camera")?;
    let invite = Invite::new_room();
    let alice_key = checks.require("alice and bob are in a room", || {
        let key = alice.join(&invite, "Alice")?.me.key.to_string();
        bob.join(&invite, "Bob")?;
        bob.wait_for("with Alice", PATIENCE, |state| names(state) == ["Alice"])?;
        // Bob says nothing, so all he hears is hers.
        bob.act(Command::SetMuted { muted: true })?;
        bob.wait_to_hear(alices, &[bobs], PATIENCE)?;
        Ok(key)
    })?;
    let mut report = Vec::new();
    checks.measured(
        "10% lost and 80 ms added: bob still hears alice's tone, dominant",
        || {
            shaper.set(&["netem", "loss", "10%", "delay", "80ms", "10ms"])?;
            let measured = listen(&mut bob, alices);
            shaper.clear()?;
            let heard = measured?;
            report.push(format!(
                "alice's tone at bob, 10% loss and 80±10 ms on her egress: {:.3} (clean is 0.5), \
                 {:.1} times the loudest other frequency, {:.0}% of the power",
                heard.level,
                heard.over_others,
                heard.share * 100.0
            ));
            if heard.level < LOUD_UNDER_LOSS || heard.over_others < DOMINANT {
                return Err(Error::Failed(format!(
                    "the tone at {:.3}, {:.1} times the loudest other; wanted {LOUD_UNDER_LOSS} and \
                     {DOMINANT} times",
                    heard.level, heard.over_others
                )));
            }
            Ok(())
        },
    );
    checks.measured(
        "30% lost for 3 s: alice's camera at bob's end recovers once it stops",
        || {
            super::video::sees(&mut bob, &alice_key, pattern)?;
            shaper.set(&["netem", "loss", "30%"])?;
            let lossy = pictures(&mut bob, &alice_key)?;
            std::thread::sleep(Duration::from_secs(3));
            shaper.clear()?;
            let during = pictures(&mut bob, &alice_key)? - lossy;
            let stopped = Instant::now();
            let before = pictures(&mut bob, &alice_key)?;
            let deadline = stopped + PATIENCE;
            loop {
                let now = pictures(&mut bob, &alice_key)?;
                if now >= before + RECOVERED {
                    break;
                }
                if Instant::now() > deadline {
                    return Err(Error::Failed(format!(
                        "{} of her pictures in {}s since the loss stopped",
                        now - before,
                        PATIENCE.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let took = stopped.elapsed();
            super::video::sees(&mut bob, &alice_key, pattern)?;
            report.push(format!(
                "alice's camera at bob, 3 s at 30% loss: {:.1} pictures a second during it (15 \
                 sent), {RECOVERED} pictures {:.1} s after it stopped",
                during as f64 / 3.0,
                took.as_secs_f64()
            ));
            Ok(())
        },
    );
    checks.record(
        "UDP blocked: alice joins over the WebSocket fallback, and bob hears her",
        || {
            alice.reset()?;
            shaper.block_udp()?;
            let joined = (|| {
                let started = Instant::now();
                alice.join(&invite, "Alice")?;
                report.push(format!(
                    "alice's join with UDP blocked, over the WebSocket fallback: {:.1} s",
                    started.elapsed().as_secs_f64()
                ));
                bob.wait_to_hear(alices, &[bobs], PATIENCE)
            })();
            let cleared = shaper.clear();
            joined.and(cleared)
        },
    );
    checks.record(
        "UDP blocked and the fallback opted out of: the join fails, and says why",
        || {
            shaper.block_udp()?;
            let ran = the_box.exec(
                &Pod::Client("alice").container("client"),
                &[
                    "timeout",
                    "40",
                    "/opt/mic/mic-crowd",
                    "--relay",
                    "https://mic-testbox-relay:4443/",
                    "--ca",
                    "/keys/ca.pem",
                    "--harness",
                    "/tmp/quic-only",
                    "--deaf",
                    "--no-websocket",
                    "--join",
                    &invite.to_mic_url(),
                    "quic-only",
                ],
            );
            shaper.clear()?;
            let ran = ran?;
            let said = String::from_utf8_lossy(&ran.stderr).into_owned();
            if ran.status.success() || !said.contains("the WebSocket fallback is off") {
                return Err(Error::Failed(format!(
                    "a QUIC-only join with UDP blocked: {}, saying {:?}",
                    ran.status,
                    said.lines().last().unwrap_or_default()
                )));
            }
            report.push(format!(
                "a QUIC-only join with UDP blocked: {}",
                said.lines().last().unwrap_or_default().trim()
            ));
            Ok(())
        },
    );
    let report = report.join("\n") + "\n";
    print!("{report}");
    crate::error::write(&checks.record_directory().join("network.txt"), &report)?;
    shaper.clear()?;
    for participant in [&mut alice, &mut bob] {
        participant.reset()?;
    }
    // A fresh alice: once a WebSocket won the race for a relay, moq-tokio gives QUIC no head
    // start there again for as long as the process lives, and she'd stay on TCP
    // (docs/observations.md).
    drop(alice);
    the_box.play(Pod::Client("alice"))?;
    crate::ready::display_up(the_box, "alice")?;
    crate::ready::client_up(the_box, "alice")
}

/// alice's pod's network, as its `shaper` sidecar sets it.
struct Shaper<'a> {
    the_box: &'a TestBox,
}

impl Shaper<'_> {
    fn run(&self, command: &[&str]) -> Result<std::process::Output> {
        self.the_box
            .exec(&Pod::Client("alice").container("shaper"), command)
    }

    fn succeed(&self, command: &[&str]) -> Result<()> {
        let ran = self.run(command)?;
        if ran.status.success() {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "{command:?} in alice's shaper: {}",
                String::from_utf8_lossy(&ran.stderr).trim()
            )))
        }
    }

    /// `qdisc` on what alice's pod sends, in place of whatever was there.
    fn set(&self, qdisc: &[&str]) -> Result<()> {
        let command: Vec<&str> = ["tc", "qdisc", "replace", "dev", "eth0", "root"]
            .into_iter()
            .chain(qdisc.iter().copied())
            .collect();
        self.succeed(&command)
    }

    /// No UDP in or out of alice's pod but DNS: QUIC can't reach the relay, though its name
    /// still resolves.
    fn block_udp(&self) -> Result<()> {
        self.succeed(&[
            "nft",
            "add table inet shaper; \
             add chain inet shaper out { type filter hook output priority 0; }; \
             add chain inet shaper in { type filter hook input priority 0; }; \
             add rule inet shaper out udp dport != 53 drop; \
             add rule inet shaper in udp sport != 53 drop",
        ])
    }

    /// The network as it was: no qdisc of the shaper's, no rules.
    fn clear(&self) -> Result<()> {
        // Either may not be there; that's as clear as it gets.
        self.run(&["tc", "qdisc", "del", "dev", "eth0", "root"])?;
        self.run(&["nft", "delete table inet shaper"])?;
        Ok(())
    }
}

/// What a listener heard of a tone.
struct Heard {
    level: f64,
    /// How many times louder than the loudest of [`OTHERS`].
    over_others: f64,
    /// Its share of what's heard, by power.
    share: f64,
}

/// What `listener` hears of `tone` over five seconds: the medians of ten looks.
fn listen(listener: &mut Participant, tone: f64) -> Result<Heard> {
    // The loss takes hold first.
    std::thread::sleep(Duration::from_secs(1));
    let frequencies: Vec<f64> = std::iter::once(tone).chain(OTHERS).collect();
    let (mut levels, mut over, mut shares) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..10 {
        let heard = listener.heard(&frequencies)?;
        let (level, others) = heard.levels.split_first().ok_or("no levels")?;
        levels.push(*level);
        let loudest = others.iter().copied().fold(1e-6, f64::max);
        over.push(level / loudest);
        // A sine of amplitude A carries A²/2 of power.
        shares.push(if heard.rms > 0.0 {
            (level * level / 2.0 / (heard.rms * heard.rms)).min(1.0)
        } else {
            0.0
        });
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(Heard {
        level: median(&mut levels),
        over_others: median(&mut over),
        share: median(&mut shares),
    })
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// How many of `whose` camera's pictures `viewer` decoded.
fn pictures(viewer: &mut Participant, whose: &str) -> Result<u64> {
    Ok(viewer
        .seen()?
        .cameras
        .iter()
        .find(|camera| camera.key == whose)
        .map_or(0, |camera| camera.pictures))
}
