//! Phases on the box as it is: `test` runs one, `run` many in rounds, `dev` is the inner loop.
//! No phase needs a fresh box: each check uses rooms and broadcasts of its own
//! (`process::unique`), so phases run one after another, or at once (`--jobs`), on one box.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::Instant;

use crate::builder;
use crate::checks::{self, Phase, Record};
use crate::error::{Error, Result};
use crate::podman::{CLIENTS, Pod, RECORD, TestBox};
use crate::{process, ready};

/// `phase`'s checks, recorded in `target/testbox-record/<record>`; each line of output starts
/// with `prefix`.
pub fn test(
    the_box: &TestBox,
    phase: Phase,
    record: &str,
    screenshots: bool,
    prefix: &str,
) -> Result<()> {
    let record = Record::new(the_box, record, screenshots)?;
    // Alone, or beside the other phases that don't disrupt the box.
    let _alone = phase
        .runs_alone()
        .then(|| THE_BOX.write().unwrap_or_else(PoisonError::into_inner));
    let _beside =
        (!phase.runs_alone()).then(|| THE_BOX.read().unwrap_or_else(PoisonError::into_inner));
    let _clients = phase.uses_clients().then(|| {
        CLIENTS_IN_USE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    });
    checks::phase(the_box, record, prefix, |checks| phase.run(checks, the_box))
}

/// Held by a phase while it drives the box's clients (`Phase::uses_clients`).
static CLIENTS_IN_USE: Mutex<()> = Mutex::new(());
/// Held for writing by a phase that disrupts the box (`Phase::runs_alone`), for reading by the
/// rest.
static THE_BOX: RwLock<()> = RwLock::new(());

/// Each of `phases`, or all, `rounds` times, up to `jobs` at once; then how each went. Fails if
/// any did. `fresh` restarts every client before each round, as `Reset` can let state leak from
/// one phase into the next.
pub fn run(
    the_box: &TestBox,
    phases: &[Phase],
    rounds: u32,
    jobs: usize,
    fresh: bool,
) -> Result<()> {
    let phases = if phases.is_empty() {
        &Phase::ROUNDS[..]
    } else {
        phases
    };
    // A directory of its own: an earlier run's records stay.
    let run = format!("run/{}", process::unique("run")?);
    let mut outcomes = Vec::new();
    for round in 1..=rounds {
        if fresh {
            println!("== round {round}: restarting the clients");
            restart_clients(the_box)?;
        }
        let queue = Mutex::new(phases.iter().copied().collect::<VecDeque<_>>());
        let done = Mutex::new(Vec::new());
        let prefixed = jobs > 1 && phases.len() > 1;
        std::thread::scope(|scope| {
            for _ in 0..jobs.max(1) {
                scope.spawn(|| {
                    while let Some(phase) = next(&queue) {
                        let name = phase.name();
                        let prefix = if prefixed {
                            format!("[{name}] ")
                        } else {
                            String::new()
                        };
                        println!("== round {round}: {name}");
                        let record = format!("{run}/{round}-{name}");
                        let outcome = test(the_box, phase, &record, false, &prefix);
                        if let Err(reason) = &outcome {
                            println!("== round {round}: {name}: {reason}");
                        }
                        if let Ok(mut done) = done.lock() {
                            done.push((round, name, outcome.is_ok()));
                        }
                    }
                });
            }
        });
        let mut done = done.into_inner().unwrap_or_default();
        done.sort();
        outcomes.extend(done);
    }
    println!(
        "== summary (records in {})",
        the_box.repo().join(RECORD).join(&run).display()
    );
    for (round, name, passed) in &outcomes {
        let outcome = if *passed { "passed" } else { "FAILED" };
        println!("round {round} {name}: {outcome}");
    }
    let failed = outcomes.iter().filter(|(_, _, passed)| !passed).count();
    if failed == 0 {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "{failed} of {} failed",
            outcomes.len()
        )))
    }
}

fn next(queue: &Mutex<VecDeque<Phase>>) -> Option<Phase> {
    queue.lock().ok()?.pop_front()
}

/// The inner loop: the box up if it isn't, what the box runs built, the pods restarted whose
/// binary changed, then `phase`; the box stays up. Says how long it all took.
pub fn dev(the_box: &TestBox, phase: Phase) -> Result<()> {
    let started = Instant::now();
    let rebuilt_images = !the_box.images()?.is_empty();
    let running = the_box.pods().into_iter().all(|pod| {
        the_box
            .containers_of(pod)
            .iter()
            .any(|container| the_box.running(container))
    });
    if rebuilt_images || !running {
        the_box.up()?;
    } else {
        let built = builder::build(the_box)?;
        let mut pods: Vec<Pod> = Vec::new();
        // Each pod once, however many of its binaries changed, in whatever order.
        for pod in built
            .updated
            .iter()
            .flat_map(|binary| builder::pods_running(binary))
        {
            if !pods.contains(&pod) {
                pods.push(pod);
            }
        }
        if !pods.is_empty() {
            for pod in pods {
                the_box.play(pod)?;
            }
            ready::all(the_box)?;
        }
    }
    let ready = started.elapsed();
    let outcome = test(the_box, phase, &phase.name(), false, "");
    println!(
        "dev {}: {:.1}s, the box ready after {:.1}s",
        phase.name(),
        started.elapsed().as_secs_f64(),
        ready.as_secs_f64()
    );
    outcome
}

/// Plays every client's pod, and the probe's, again, and waits until each is up.
fn restart_clients(the_box: &TestBox) -> Result<()> {
    for client in CLIENTS {
        the_box.play(Pod::Client(client))?;
    }
    if the_box.crowd() > 0 {
        the_box.play(Pod::Crowd)?;
    }
    the_box.play(Pod::Probe)?;
    ready::all(the_box)
}
