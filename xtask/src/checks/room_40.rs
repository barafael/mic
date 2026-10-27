//! The `room-40` phase (PLAN.md, commit 48): forty in one room, the six windows and 34 of the
//! crowd, for three minutes, measured each minute against §5.8's budget and this host's
//! baseline, taken first with alice alone. alice speaks, so the windows have a speaker; the
//! crowd decodes and plays nothing (testbox/pods/crowd.yaml), so what's measured is the windows'
//! and the relay's.
//!
//! Each minute: alice's mic's CPU and memory, all six windows' CPU, the crowd's CPU, the GPU's
//! busy share (the host's, which its desktop shares), and the relay's egress. It fails past the
//! budget: the relay's egress over §5.8's 130 Mbit/s for 40, a window over a core, or alice's mic
//! over 1 GiB. The record keeps it all (`room-40.txt`). It measures, so it runs alone, and isn't
//! in a default round: `cargo testbox run room-40`.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite};

use crate::error::{Error, Result};
use crate::harness::Participant;
use crate::podman::{CLIENTS, Pod, TestBox, crowd_name};
use crate::view;

use super::Checks;

/// How many are in the room.
const EVERYONE: usize = 40;
/// How long each measurement is, and how many of them.
const MINUTE: Duration = Duration::from_secs(60);
const MINUTES: u32 = 3;
/// How long the baseline is.
const BASELINE: Duration = Duration::from_secs(30);
/// How long a roster may take to fill.
const PATIENCE: Duration = Duration::from_secs(60);
/// §5.8's egress for a room of 40, in Mbit/s.
const EGRESS_BUDGET: f64 = 130.0;
/// The most of a core a window may take, and memory alice's mic may hold, in MiB.
const CPU_BUDGET: f64 = 1.0;
const MEMORY_BUDGET: f64 = 1024.0;

pub fn room_40(checks: &mut Checks, the_box: &TestBox) -> Result<()> {
    let crowd_size = EVERYONE - CLIENTS.len();
    if (the_box.crowd() as usize) < crowd_size {
        return Err(Error::Failed(format!(
            "a crowd of {} in the box, of {crowd_size} wanted: `cargo testbox up --crowd {crowd_size}`",
            the_box.crowd()
        )));
    }
    let mut windows = checks.require("the windows answer", || {
        CLIENTS
            .iter()
            .map(|client| Participant::connect(the_box, client))
            .collect::<Result<Vec<_>>>()
    })?;
    let mut crowd = checks.require("the crowd answers", || {
        (1..=crowd_size as u32)
            .map(|index| Participant::crowd(the_box, index))
            .collect::<Result<Vec<_>>>()
    })?;
    // In no room, whatever a phase before left: the baseline is alice's alone.
    for participant in windows.iter_mut().chain(crowd.iter_mut()) {
        participant.reset()?;
    }
    let mut lines = vec![format!(
        "room-40: {} windows and {crowd_size} of the crowd, {MINUTES} minutes",
        CLIENTS.len()
    )];
    let baseline = checks.require("the baseline: alice alone in a room", || {
        windows[0].join(&Invite::new_room(), "Alice")?;
        let sample = measure(the_box, BASELINE)?;
        windows[0].reset()?;
        Ok(sample)
    })?;
    lines.push(format!("baseline, alice alone: {}", baseline.line()));
    let invite = Invite::new_room();
    checks.require("forty in one room", || {
        for (index, member) in crowd.iter_mut().enumerate() {
            member.join(&invite, &crowd_name(index as u32 + 1))?;
        }
        for window in &mut windows {
            let name = window.name.clone();
            window.join(&invite, &name)?;
            if name != "alice" {
                window.act(Command::SetMuted { muted: true })?;
            }
        }
        for window in &mut windows {
            window.wait_for("with everyone", PATIENCE, |state| {
                state.participants.len() == EVERYONE - 1
            })?;
            window.wait_for_snapshot("nine tiles and a +31", PATIENCE, |snapshot| {
                snapshot
                    .ui
                    .as_ref()
                    .is_some_and(|ui| ui.tiles.len() == 9 && ui.overflow == EVERYONE - 9)
            })?;
        }
        Ok(())
    })?;
    let mut samples = Vec::new();
    for minute in 1..=MINUTES {
        checks.record(&format!("minute {minute} within the budget"), || {
            let sample = measure(the_box, MINUTE)?;
            lines.push(format!("minute {minute}: {}", sample.line()));
            let within = sample.relay_mbps <= EGRESS_BUDGET
                && sample.windows_cpu.iter().all(|cpu| *cpu <= CPU_BUDGET)
                && sample.alice_mib <= MEMORY_BUDGET;
            samples.push(sample);
            if within {
                Ok(())
            } else {
                Err(Error::Failed(format!(
                    "over the budget ({EGRESS_BUDGET} Mbit/s, {:.0}% of a core a window, \
                     {MEMORY_BUDGET} MiB): {}",
                    CPU_BUDGET * 100.0,
                    lines.last().cloned().unwrap_or_default()
                )))
            }
        });
    }
    let report = lines.join("\n") + "\n";
    print!("{report}");
    crate::error::write(&checks.record_directory().join("room-40.txt"), &report)?;
    for participant in windows.iter_mut().chain(crowd.iter_mut()) {
        participant.reset()?;
    }
    Ok(())
}

/// What a stretch of time cost.
struct Sample {
    /// Of a core, alice's mic, and each window's.
    alice_cpu: f64,
    windows_cpu: Vec<f64>,
    crowd_cpu: f64,
    /// alice's mic's resident memory at the end, in MiB.
    alice_mib: f64,
    /// The host GPU's busy share, on average, if it says.
    gpu_busy: Option<f64>,
    /// What the relay sent, in Mbit/s.
    relay_mbps: f64,
}

impl Sample {
    fn line(&self) -> String {
        let windows: f64 = self.windows_cpu.iter().sum();
        format!(
            "alice {:.0}% of a core, {:.0} MiB; the six windows {:.0}% (the most {:.0}%); the crowd \
             {:.0}%; the GPU {}; the relay sends {:.1} Mbit/s",
            self.alice_cpu * 100.0,
            self.alice_mib,
            windows * 100.0,
            self.windows_cpu.iter().copied().fold(0.0, f64::max) * 100.0,
            self.crowd_cpu * 100.0,
            self.gpu_busy
                .map_or("unknown".to_owned(), |busy| format!("{busy:.0}% busy")),
            self.relay_mbps
        )
    }
}

/// What the next `stretch` costs.
fn measure(the_box: &TestBox, stretch: Duration) -> Result<Sample> {
    let windows_before = CLIENTS
        .iter()
        .map(|client| process(the_box, &view::container(client), "mic"))
        .collect::<Result<Vec<_>>>()?;
    let crowd_before = process(the_box, &Pod::Crowd.container("crowd"), "mic-crowd")?;
    let sent_before = relay_sent(the_box)?;
    let started = Instant::now();
    let mut busy = Vec::new();
    while started.elapsed() < stretch {
        if let Some(sampled) = gpu_busy() {
            busy.push(sampled);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let seconds = started.elapsed().as_secs_f64();
    let windows_after = CLIENTS
        .iter()
        .map(|client| process(the_box, &view::container(client), "mic"))
        .collect::<Result<Vec<_>>>()?;
    let crowd_after = process(the_box, &Pod::Crowd.container("crowd"), "mic-crowd")?;
    let sent = relay_sent(the_box)? - sent_before;
    let windows_cpu: Vec<f64> = windows_before
        .iter()
        .zip(&windows_after)
        .map(|(before, after)| (after.cpu - before.cpu) / seconds)
        .collect();
    Ok(Sample {
        alice_cpu: windows_cpu[0],
        alice_mib: windows_after[0].rss_kib / 1024.0,
        windows_cpu,
        crowd_cpu: (crowd_after.cpu - crowd_before.cpu) / seconds,
        gpu_busy: (!busy.is_empty()).then(|| busy.iter().sum::<f64>() / busy.len() as f64),
        relay_mbps: sent as f64 * 8.0 / seconds / 1e6,
    })
}

/// A process's CPU time so far, user and system, in seconds, and its resident memory in KiB.
struct Process {
    cpu: f64,
    rss_kib: f64,
}

/// `name`'s process in `container`, from /proc: its stat's utime and stime, in Linux's clock
/// ticks of a hundredth of a second, and its status's VmRSS.
fn process(the_box: &TestBox, container: &str, name: &str) -> Result<Process> {
    let read = the_box.exec(
        container,
        &[
            "sh",
            "-c",
            &format!(
                "pid=$(pgrep -x {name}) && cat /proc/$pid/stat && grep VmRSS /proc/$pid/status"
            ),
        ],
    )?;
    let said = String::from_utf8_lossy(&read.stdout);
    let not = || Error::Failed(format!("{name} in {container}: {said:?}"));
    let (stat, status) = said.split_once('\n').ok_or_else(not)?;
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    let ticks = |at: usize| fields.get(at).and_then(|field| field.parse::<f64>().ok());
    let cpu = ticks(11).zip(ticks(12)).ok_or_else(not)?;
    let rss_kib = status
        .split_whitespace()
        .nth(1)
        .and_then(|kib| kib.parse::<f64>().ok())
        .ok_or_else(not)?;
    Ok(Process {
        cpu: (cpu.0 + cpu.1) / 100.0,
        rss_kib,
    })
}

/// What the relay's pod has sent, in bytes, by its interface's count.
fn relay_sent(the_box: &TestBox) -> Result<u64> {
    let read = the_box.exec(
        &Pod::Relay.container("relay"),
        &["cat", "/sys/class/net/eth0/statistics/tx_bytes"],
    )?;
    let said = String::from_utf8_lossy(&read.stdout);
    said.trim()
        .parse()
        .map_err(|_| Error::Failed(format!("the relay's tx_bytes: {said:?}")))
}

/// The host GPU's busy share now, in percent, where its driver says (amdgpu's
/// `gpu_busy_percent`).
fn gpu_busy() -> Option<f64> {
    std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .find_map(|card| {
            std::fs::read_to_string(card.path().join("device/gpu_busy_percent"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
}
