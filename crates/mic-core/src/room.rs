//! A room joined, as frontends and the harness see it: its state behind a `watch`, events as they
//! happen, and commands (PLAN.md, "The core and its frontends"). A [`Room`] is a cheap handle to
//! a task of the room's own, which owns the session, this participant's broadcast and the
//! roster; a command is answered over a oneshot channel, so any executor can await it.
//!
//! Dropping every handle leaves the room, as [`Command::Leave`] does.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::invite::{Invite, RoomId};
use crate::roster::{self, Participant, ParticipantKey, Profile, Rejected, Roster};
use crate::session::Error;
use crate::tracks::{Peers, Publication, RoomCrypto};

/// How many events a slow reader may fall behind by before it misses some.
const EVENTS: usize = 256;

/// What a frontend shows of a room: plain data, no toolkit types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    /// The room's id, as an invite names it.
    pub room: String,
    /// This participant.
    pub me: Participant,
    /// The others, in the order they joined.
    pub participants: Vec<Participant>,
    /// Broadcasts in the room whose signed catalog doesn't stand: someone publishing what they
    /// can't sign, as when they pose as someone else. A frontend warns of them.
    pub rejected: Vec<Rejected>,
    pub connection: Connection,
}

/// The room's session with the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    Connected,
    /// The session dropped, and the room is dialing the relay again, with backoff, for as long
    /// as it takes or until it's left. Its broadcast comes back as it was.
    Reconnecting,
    /// Left: the room is over for this participant, and no command does anything any more.
    Left,
}

/// What a frontend or the harness asks of a room. Clicks and harness actions are both these, so
/// they take the same path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Leave the room: the others see this participant go.
    Leave,
    /// Be called this, for everyone in the room.
    SetName { name: String },
    /// Send no voice, or send it again; everyone's tile of this participant says which.
    SetMuted { muted: bool },
    /// Take the noise out of the voice with this suppressor, or none.
    SetNoiseSuppression {
        mode: mic_media::audio::noise::NoiseSuppression,
    },
}

/// One-off things that happen in a room, beside its state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Someone joined.
    Arrived { participant: Participant },
    /// Someone left.
    Departed { participant: Participant },
    /// A broadcast in the room didn't stand, and nothing of it is shown.
    Rejected { rejected: Rejected },
}

/// A room joined. Cheap to clone; every clone is the same room.
#[derive(Clone)]
pub struct Room {
    shared: Arc<Shared>,
    requests: mpsc::UnboundedSender<Request>,
    state: watch::Receiver<RoomState>,
    /// Weak, so the events end once the room's task does, while handles remain.
    events: broadcast::WeakSender<Event>,
}

/// What every handle reads without asking the room's task.
struct Shared {
    invite: Invite,
    participant: ParticipantKey,
    crypto: RoomCrypto,
    /// This participant's broadcast's path under the room's prefix.
    path: String,
    origin: moq_net::origin::Consumer,
    runtime: tokio::runtime::Handle,
    /// What the others' cameras show.
    board: crate::video::Board,
}

/// What a handle asks of the room's task.
enum Request {
    Command(Command, oneshot::Sender<Result<(), Error>>),
    Publish(
        String,
        oneshot::Sender<Result<crate::sig::SignedTrack, Error>>,
    ),
}

/// What a joined session hands to its room.
pub(crate) struct Joined {
    pub invite: Invite,
    pub participant: ParticipantKey,
    pub crypto: RoomCrypto,
    pub path: String,
    pub publication: Publication,
    pub profile: Profile,
    pub profile_track: crate::sig::SignedTrack,
    pub connection: moq_tokio::Connection,
    /// What the room dials again with, once a connection gives up.
    pub moq: moq_tokio::Client,
    pub url: url::Url,
    pub origin: moq_net::origin::Producer,
    pub runtime: tokio::runtime::Handle,
    /// Sends this participant's voice, if it has one.
    pub voice: Option<tokio::task::JoinHandle<()>>,
    /// Sends its camera, if it has one.
    pub camera: Option<tokio::task::JoinHandle<()>>,
    /// Whether the voice is muted, which it follows.
    pub muted: watch::Sender<bool>,
    /// What suppresses the voice's noise, which it follows.
    pub noise: watch::Sender<mic_media::audio::noise::NoiseSuppression>,
    /// Where the others' voices play, if anywhere.
    pub mixer: Option<mic_media::audio::Mixer>,
    /// The video codecs this participant decodes, by name.
    pub decoders: Vec<String>,
}

impl Room {
    /// The room of a session just joined, its task started on the session's runtime.
    pub(crate) fn start(joined: Joined) -> Self {
        let me = Participant {
            key: joined.participant,
            profile: joined.profile.clone(),
            decoders: joined.decoders.clone(),
            renditions: Vec::new(),
        };
        let (state, watched) = watch::channel(RoomState {
            room: joined.invite.room.to_string(),
            me,
            participants: Vec::new(),
            rejected: Vec::new(),
            connection: Connection::Connected,
        });
        let (events, _) = broadcast::channel(EVENTS);
        let weak_events = events.downgrade();
        let (requests, received) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            invite: joined.invite,
            participant: joined.participant,
            crypto: joined.crypto.clone(),
            path: joined.path.clone(),
            origin: joined.origin.consume(),
            runtime: joined.runtime.clone(),
            board: crate::video::Board::default(),
        });
        let (roster, followed) = watch::channel(Roster::default());
        let peers = Peers::new(joined.crypto.clone(), joined.path, joined.origin.consume());
        let board = shared.board.clone();
        let following = joined.runtime.spawn(roster::follow(
            peers,
            joined.crypto,
            shared.invite.room,
            roster,
            roster::Playing {
                mixer: joined.mixer,
                board: board.clone(),
                decoders: joined.decoders.clone(),
            },
        ));
        let task = Task {
            publication: joined.publication,
            profile: joined.profile,
            profile_track: Some(joined.profile_track),
            connection: Some(joined.connection),
            moq: joined.moq,
            url: joined.url,
            origin: joined.origin,
            following,
            voice: joined.voice,
            camera: joined.camera,
            muted: joined.muted,
            noise: joined.noise,
            roster: followed,
            state,
            events,
        };
        joined.runtime.spawn(task.run(received));
        Self {
            shared,
            requests,
            state: watched,
            events: weak_events,
        }
    }

    /// The room's state, kept up to date.
    pub fn state(&self) -> watch::Receiver<RoomState> {
        self.state.clone()
    }

    /// What happens from now on.
    pub fn events(&self) -> Events {
        match self.events.upgrade() {
            Some(events) => Events(events.subscribe()),
            // Left: events that are over already.
            None => Events(broadcast::channel(1).1),
        }
    }

    /// Does what `command` says; done once this returns. After the room is left, every command
    /// is answered with [`Error::Left`].
    pub async fn command(&self, command: Command) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Command(command, reply))
            .map_err(|_| Error::Left)?;
        answer.await.map_err(|_| Error::Left)?
    }

    pub fn invite(&self) -> &Invite {
        &self.shared.invite
    }

    pub fn id(&self) -> RoomId {
        self.shared.invite.room
    }

    /// The key others know this participant by.
    pub fn participant(&self) -> ParticipantKey {
        self.shared.participant
    }

    /// This participant's broadcast's path under the room's prefix.
    pub fn path(&self) -> &str {
        &self.shared.path
    }

    /// A track `semantic` of this participant's broadcast, encrypted.
    pub async fn publish(&self, semantic: &str) -> Result<crate::sig::SignedTrack, Error> {
        let (reply, answer) = oneshot::channel();
        self.requests
            .send(Request::Publish(semantic.to_owned(), reply))
            .map_err(|_| Error::Left)?;
        answer.await.map_err(|_| Error::Left)?
    }

    /// `whose` camera's pictures, as they're decoded: none until the first, nor ever for a
    /// camera this client can't decode or a participant with none.
    pub fn video(&self, whose: ParticipantKey) -> crate::video::Pictures {
        self.shared.board.pictures(whose)
    }

    /// How many of `whose` camera's pictures were decoded.
    pub fn video_count(&self, whose: ParticipantKey) -> u64 {
        self.shared.board.count(whose)
    }

    /// The other participants' broadcasts as they arrive and go, those there already first.
    pub fn peers(&self) -> Peers {
        let _entered = self.shared.runtime.enter();
        Peers::new(
            self.shared.crypto.clone(),
            self.shared.path.clone(),
            self.shared.origin.clone(),
        )
    }
}

/// A room's events, from when they were asked for.
pub struct Events(broadcast::Receiver<Event>);

impl Events {
    /// The next event; none once the room is left. A reader that fell more than 256 events
    /// behind misses the oldest of them.
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            match self.0.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "a reader of a room's events fell behind");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// The room's own task.
struct Task {
    publication: Publication,
    profile: Profile,
    /// Until the room is left.
    profile_track: Option<crate::sig::SignedTrack>,
    /// Until the room is left.
    connection: Option<moq_tokio::Connection>,
    moq: moq_tokio::Client,
    url: url::Url,
    /// What the session publishes and subscribes to; a new connection takes it up as it was.
    origin: moq_net::origin::Producer,
    following: tokio::task::JoinHandle<()>,
    /// Sends the voice, until the room is left.
    voice: Option<tokio::task::JoinHandle<()>>,
    /// Sends the camera, until the room is left.
    camera: Option<tokio::task::JoinHandle<()>>,
    muted: watch::Sender<bool>,
    noise: watch::Sender<mic_media::audio::noise::NoiseSuppression>,
    roster: watch::Receiver<Roster>,
    state: watch::Sender<RoomState>,
    events: broadcast::Sender<Event>,
}

impl Task {
    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        let mut roster_open = true;
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(Request::Command(command, reply)) => {
                        let left = command == Command::Leave;
                        // The asker may have gone; then nobody needs the answer.
                        let _unasked: Result<(), _> = reply.send(self.command(command).await);
                        if left {
                            break;
                        }
                    }
                    Some(Request::Publish(semantic, reply)) => {
                        let track = self.publication.track(&semantic).map_err(Error::from);
                        let _unasked: Result<(), _> = reply.send(track);
                    }
                    // Every handle dropped: the room is left as if asked to.
                    None => {
                        self.leave().await;
                        break;
                    }
                },
                // Once the roster's task is gone, its `changed` is ready at once, every time: then
                // it's no longer asked, or the loop would spin.
                changed = self.roster.changed(), if roster_open => {
                    if changed.is_err() {
                        roster_open = false;
                        continue;
                    }
                    let roster = self.roster.borrow_and_update().clone();
                    self.roster_changed(roster);
                }
                status = status(&mut self.connection) => self.status(status),
            }
        }
    }

    async fn command(&mut self, command: Command) -> Result<(), Error> {
        match command {
            Command::Leave => {
                self.leave().await;
                Ok(())
            }
            Command::SetName { name } => self.set_profile(Profile {
                muted: self.profile.muted,
                ..Profile::new(&name)
            }),
            Command::SetMuted { muted } => {
                self.set_profile(Profile {
                    muted,
                    ..self.profile.clone()
                })?;
                self.muted.send_replace(muted);
                Ok(())
            }
            Command::SetNoiseSuppression { mode } => {
                self.noise.send_replace(mode);
                Ok(())
            }
        }
    }

    /// `profile`, for everyone in the room, and in the state.
    fn set_profile(&mut self, profile: Profile) -> Result<(), Error> {
        let track = self.profile_track.as_mut().ok_or(Error::Left)?;
        roster::write_profile(track, &profile)?;
        self.profile = profile.clone();
        self.state.send_modify(|state| state.me.profile = profile);
        Ok(())
    }

    /// What the connection said into the state. A connection gives up after its redial window;
    /// the room then dials a new one on the same origin, and so goes on until it's left.
    fn status(&mut self, status: moq_tokio::Result<moq_tokio::Status>) {
        let connection = match status {
            Ok(moq_tokio::Status::Connected) => Connection::Connected,
            Ok(moq_tokio::Status::Disconnected) => Connection::Reconnecting,
            // A GOAWAY (the old session serves while the new one is dialed), or a status a later
            // moq-tokio adds: the connection as it was.
            Ok(_) => return,
            Err(error) => {
                tracing::info!(%error, "the connection gave up; dialing again");
                self.connection = Some(
                    self.moq
                        .clone()
                        .with_origin(self.origin.clone())
                        .connect(self.url.clone()),
                );
                Connection::Reconnecting
            }
        };
        self.state.send_if_modified(|state| {
            let changed = state.connection != connection;
            state.connection = connection;
            changed
        });
    }

    /// The new roster into the state, and who came and went, and what didn't stand, as events.
    fn roster_changed(&self, roster: Roster) {
        let (before, rejected_before) = {
            let state = self.state.borrow();
            (state.participants.clone(), state.rejected.clone())
        };
        let Roster {
            participants: roster,
            rejected,
        } = roster;
        let known = |list: &[Participant], participant: &Participant| {
            list.iter().any(|other| other.key == participant.key)
        };
        let arrived = roster.iter().filter(|now| !known(&before, now)).cloned();
        let departed = before.iter().filter(|then| !known(&roster, then)).cloned();
        // Nobody listening to events is fine: the state has it all.
        let newly_rejected = rejected
            .iter()
            .filter(|now| !rejected_before.contains(now))
            .cloned();
        for event in arrived
            .map(|participant| Event::Arrived { participant })
            .chain(departed.map(|participant| Event::Departed { participant }))
            .chain(newly_rejected.map(|rejected| Event::Rejected { rejected }))
        {
            let _unheard: Result<usize, _> = self.events.send(event);
        }
        self.state.send_modify(|state| {
            state.participants = roster;
            state.rejected = rejected;
        });
    }

    /// Finishes this participant's tracks and closes its broadcast, so the others see it go at
    /// once, then closes the session once what it queued is delivered, for up to a second. One
    /// that doesn't drain in time is closed all the same.
    async fn leave(&mut self) {
        self.following.abort();
        for sending in [self.voice.take(), self.camera.take()]
            .into_iter()
            .flatten()
        {
            sending.abort();
        }
        let Some(connection) = self.connection.take() else {
            return;
        };
        if let Some(track) = self.profile_track.take()
            && let Err(error) = track.finish()
        {
            tracing::debug!(%error, "finishing the profile track");
        }
        self.publication.close();
        if let Err(error) = connection.close().await {
            tracing::debug!(%error, "the session didn't drain before it closed");
        }
        self.state.send_modify(|state| {
            state.participants.clear();
            state.connection = Connection::Left;
        });
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.following.abort();
        for sending in [&self.voice, &self.camera].into_iter().flatten() {
            sending.abort();
        }
    }
}

/// The next change of `connection`'s status; never, once there's no connection.
async fn status(
    connection: &mut Option<moq_tokio::Connection>,
) -> moq_tokio::Result<moq_tokio::Status> {
    match connection {
        Some(connection) => connection.status().await,
        None => std::future::pending().await,
    }
}
