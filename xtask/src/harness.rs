//! A participant of the box, driven through its harness (crates/mic-harness) on its socket in
//! target/testbox-run/<client>/, as a check drives it: join a room, act, read what it shows.

use std::time::{Duration, Instant};

use mic_core::{Command, Invite, RoomState};
use mic_harness::client::Client;
use mic_harness::{Acted, Call, Done, Joined, Pong, Snapshot};

use crate::error::{Error, Result};
use crate::podman::TestBox;

/// How long a reply may take: a join waits for its session.
const REPLY: Duration = Duration::from_secs(30);
/// How often a state is looked at again while waiting for it.
const POLL: Duration = Duration::from_millis(100);

pub struct Participant {
    pub name: &'static str,
    client: Client,
}

impl Participant {
    /// `client`'s harness, connected.
    pub fn connect(the_box: &TestBox, client: &'static str) -> Result<Self> {
        let socket = the_box.harness_socket(client);
        let connected = Client::connect(&socket, REPLY).map_err(|source| Error::Io {
            what: socket.display().to_string(),
            source,
        })?;
        Ok(Self {
            name: client,
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

    /// The room's state once `holds` says so, within `patience`; fails saying what it was, as
    /// `what` names what was waited for.
    pub fn wait_for(
        &mut self,
        what: &str,
        patience: Duration,
        holds: impl Fn(&RoomState) -> bool,
    ) -> Result<RoomState> {
        let deadline = Instant::now() + patience;
        loop {
            let room = self.snapshot()?.room;
            match room {
                Some(state) if holds(&state) => return Ok(state),
                last if Instant::now() > deadline => {
                    return Err(Error::Failed(format!(
                        "{}: not {what} within {}s, but {}",
                        self.name,
                        patience.as_secs(),
                        describe(last.as_ref())
                    )));
                }
                _ => std::thread::sleep(POLL),
            }
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

fn describe(state: Option<&RoomState>) -> String {
    match state {
        None => "in no room".to_owned(),
        Some(state) => format!("{:?} with {:?}", state.connection, names(state)),
    }
}
