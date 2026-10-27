//! A participant of the box, driven through its harness (crates/mic-harness) on its socket in
//! target/testbox-run/<client>/, as a check drives it: join a room, act, read what it shows.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite, RoomState};
use mic_harness::client::Client;
use mic_harness::{Acted, Call, Done, Heard, Joined, Pong, Seen, Snapshot};

use crate::error::{Error, Result};
use crate::podman::TestBox;

/// How long a reply may take: a join waits for its session.
const REPLY: Duration = Duration::from_secs(30);
/// What a tone of amplitude 0.5 must be heard at, at least: half of it, which leaves room for
/// what the codec and a gap take.
pub const LOUD: f64 = 0.25;
/// What a tone not sent must stay below: a twenty-fifth of a sent one.
pub const SILENT: f64 = 0.02;
/// How often a state is looked at again while waiting for it.
const POLL: Duration = Duration::from_millis(100);

pub struct Participant {
    pub name: String,
    client: Client,
}

impl Participant {
    /// `client`'s harness, connected.
    pub fn connect(the_box: &TestBox, client: &'static str) -> Result<Self> {
        Self::at(&the_box.harness_socket(client), client)
    }

    /// The crowd's participant `index` (from 1), connected (`Pod::Crowd`).
    pub fn crowd(the_box: &TestBox, index: u32) -> Result<Self> {
        let name = crate::podman::crowd_name(index);
        Self::at(&the_box.crowd_socket(&name), &name)
    }

    fn at(socket: &std::path::Path, name: &str) -> Result<Self> {
        let connected = Client::connect(socket, REPLY).map_err(|source| Error::Io {
            what: socket.display().to_string(),
            source,
        })?;
        Ok(Self {
            name: name.to_owned(),
            client: connected,
        })
    }

    fn call<T: serde::de::DeserializeOwned>(&mut self, call: Call) -> Result<T> {
        self.client
            .call(call)
            .map_err(|error| Error::Failed(format!("{}: {error}", self.name)))
    }

    pub fn ping(&mut self) -> Result<Pong> {
        self.call(Call::Ping)
    }

    /// Joins the room `invite` names, as `name`.
    pub fn join(&mut self, invite: &Invite, name: &str) -> Result<RoomState> {
        let joined: Joined = self.call(Call::Join {
            invite: invite.to_mic_url(),
            name: name.to_owned(),
        })?;
        Ok(joined.state)
    }

    pub fn act(&mut self, command: Command) -> Result<()> {
        let _acted: Acted = self.call(Call::Act { command })?;
        Ok(())
    }

    /// Leaves the room, if any, and forgets it.
    pub fn reset(&mut self) -> Result<()> {
        let done: Done = self.call(Call::Reset)?;
        if done.ok {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "{} didn't leave its room",
                self.name
            )))
        }
    }

    pub fn snapshot(&mut self) -> Result<Snapshot> {
        self.call(Call::Snapshot)
    }

    /// What it decoded of each other participant's camera.
    pub fn seen(&mut self) -> Result<Seen> {
        self.call(Call::Seen)
    }

    /// How loud each of `frequencies` is in what it heard in the last second.
    pub fn heard(&mut self, frequencies: &[f64]) -> Result<Heard> {
        self.call(Call::Heard {
            frequencies: frequencies.to_vec(),
        })
    }

    /// Waits until it hears `tone` (Hz) at least at [`LOUD`] and each of `quiet` below [`SILENT`],
    /// within `patience`; fails saying what it heard.
    pub fn wait_to_hear(&mut self, tone: f64, quiet: &[f64], patience: Duration) -> Result<()> {
        let frequencies: Vec<f64> = std::iter::once(tone).chain(quiet.iter().copied()).collect();
        let deadline = Instant::now() + patience;
        loop {
            let heard = self.heard(&frequencies)?;
            let (loudest, rest) = heard.levels.split_first().ok_or("no levels")?;
            if *loudest >= LOUD && rest.iter().all(|&level| level < SILENT) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(Error::Failed(format!(
                    "{}: not {tone} Hz alone within {}s: {:?} at {:?}",
                    self.name,
                    patience.as_secs(),
                    frequencies,
                    heard.levels
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    /// Waits until each of `tones` is below [`SILENT`] in what it heard, within `patience`.
    pub fn wait_for_silence(&mut self, tones: &[f64], patience: Duration) -> Result<()> {
        let deadline = Instant::now() + patience;
        loop {
            let heard = self.heard(tones)?;
            if heard.levels.iter().all(|&level| level < SILENT) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(Error::Failed(format!(
                    "{}: {tones:?} still at {:?} after {}s",
                    self.name,
                    heard.levels,
                    patience.as_secs()
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    /// The room's state once `holds` says so, within `patience`; fails saying what it was, as
    /// `what` names what was waited for.
    pub fn wait_for(
        &mut self,
        what: &str,
        patience: Duration,
        holds: impl Fn(&RoomState) -> bool,
    ) -> Result<RoomState> {
        let snapshot = self.wait_for_snapshot(what, patience, |snapshot| {
            snapshot.room.as_ref().is_some_and(&holds)
        })?;
        snapshot
            .room
            .ok_or_else(|| Error::Failed(format!("{}: in no room", self.name)))
    }

    /// The snapshot once `holds` says so, within `patience`, as for [`Self::wait_for`].
    pub fn wait_for_snapshot(
        &mut self,
        what: &str,
        patience: Duration,
        holds: impl Fn(&Snapshot) -> bool,
    ) -> Result<Snapshot> {
        let deadline = Instant::now() + patience;
        loop {
            let snapshot = self.snapshot()?;
            if holds(&snapshot) {
                return Ok(snapshot);
            }
            if Instant::now() > deadline {
                return Err(Error::Failed(format!(
                    "{}: not {what} within {}s, but {}",
                    self.name,
                    patience.as_secs(),
                    describe(&snapshot)
                )));
            }
            std::thread::sleep(POLL);
        }
    }
}

/// The names a room's state shows, as a check's failure says it.
pub fn names(state: &RoomState) -> Vec<String> {
    state
        .participants
        .iter()
        .map(|participant| participant.profile.name.clone())
        .collect()
}

fn describe(snapshot: &Snapshot) -> String {
    let room = match &snapshot.room {
        None => "in no room".to_owned(),
        Some(state) => format!("{:?} with {:?}", state.connection, names(state)),
    };
    match &snapshot.ui {
        None => room,
        Some(ui) => format!(
            "{room}, showing {} with {} tiles",
            ui.screen,
            ui.tiles.len()
        ),
    }
}
