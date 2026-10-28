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
/// How long a participant gone just before the room lost its session counts as lost with it: the
/// relay's unannouncements can come before the session's end does.
const LOST_WITH_IT: std::time::Duration = std::time::Duration::from_secs(3);
/// How long, once the room is back, those it kept through a reconnect have to come back too.
const AWAY_FOR: std::time::Duration = std::time::Duration::from_secs(10);
/// How many messages the state keeps, the newest.
pub const CHAT_KEPT: usize = 500;
/// How many reactions the state keeps, the newest.
pub const REACTIONS_KEPT: usize = 32;

/// What a frontend shows of a room: plain data, no toolkit types. The default is an empty room, as
/// a test's state starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    /// The room's id, as an invite names it.
    pub room: String,
    /// What its invite calls it, if anything.
    #[serde(default)]
    pub name: Option<String>,
    /// This participant.
    pub me: Participant,
    /// The others, in the order they joined.
    pub participants: Vec<Participant>,
    /// Broadcasts in the room whose signed catalog doesn't stand: someone publishing what they
    /// can't sign, as when they pose as someone else. A frontend warns of them.
    pub rejected: Vec<Rejected>,
    pub connection: Connection,
    /// How the session reaches the relay, while it does.
    #[serde(default)]
    pub transport: Option<Transport>,
    /// The active speaker among the others (`crate::speaker`), once someone has spoken: kept
    /// after they stop, until someone else speaks.
    #[serde(default)]
    pub speaker: Option<ParticipantKey>,
    /// The others speaking now.
    #[serde(default)]
    pub speaking: Vec<ParticipantKey>,
    /// The chat since this participant joined, oldest first, its own messages among them: the
    /// newest [`CHAT_KEPT`].
    #[serde(default)]
    pub chat: Vec<Message>,
    /// The newest reactions, this participant's among them: the newest [`REACTIONS_KEPT`].
    #[serde(default)]
    pub reactions: Vec<Reacted>,
    /// A share of this participant's screen is opening: the desktop may be asking which screen.
    #[serde(default)]
    pub screen_opening: bool,
}

/// A chat message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub from: ParticipantKey,
    /// Its sender's name when it came.
    pub name: String,
    pub text: String,
}

/// A reaction, as it came.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reacted {
    /// Counts the reactions this participant saw in the room, from 0: what tells two of the same
    /// apart.
    pub seq: u64,
    pub from: ParticipantKey,
    pub reaction: crate::chat::Reaction,
}

/// How a session reaches the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Quic,
    /// The fallback over TCP, where UDP doesn't get through (D9): a frontend says so, as it
    /// carries less (the relay's stream limit, docs/observations.md).
    WebSocket,
}

/// The room's session with the relay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    #[default]
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
    /// Turn the camera off, or on again; everyone's tile of this participant says which.
    SetCamera { on: bool },
    /// Take the noise out of the voice with this suppressor, or none.
    SetNoiseSuppression {
        mode: mic_media::audio::noise::NoiseSuppression,
    },
    /// Say `text` in the chat: trimmed, and cut to `chat::TEXT_LIMIT` characters; nothing is
    /// said if nothing's left.
    Say { text: String },
    /// React, for everyone in the room.
    React { reaction: crate::chat::Reaction },
    /// Raise a hand, or lower it; everyone's tile of this participant says which.
    SetHand { raised: bool },
    /// Share the screen the screen source captures, or stop.
    ShareScreen { on: bool },
    /// Record the call into a new WebM file in the recordings (`Config::recordings`), or stop,
    /// once it's written out: a shared screen, else the active speaker, else a grid of
    /// everyone, and every voice. Everyone is told.
    Record { on: bool },
    /// Show the cameras of these participants only, as a frontend's visible tiles do; every
    /// camera with none (as at the start). Only those are subscribed to and decoded (§5.8).
    Watch {
        participants: Option<Vec<ParticipantKey>>,
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
    /// A message in the chat, this participant's own among them.
    Said { message: Message },
    /// A reaction, this participant's own among them.
    Reacted { reacted: Reacted },
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
    /// What the others' screens show.
    screens: crate::video::Board,
    /// What this participant's camera sends, if it sends one.
    preview: Option<crate::video::Pictures>,
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
    pub saying: crate::chat::Saying,
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
    /// What its camera sends, if it sends one.
    pub preview: Option<crate::video::Pictures>,
    /// Whether the voice is muted, which it follows.
    pub muted: watch::Sender<bool>,
    /// Whether the camera is on, which it follows.
    pub camera_on: watch::Sender<bool>,
    /// What a screenshare captures, if this participant can share one.
    pub screen: Option<crate::video::ScreenSource>,
    /// What a recording takes this participant's voice from, and the others'.
    pub tap: crate::recording::Tap,
    /// Where recordings go.
    pub recordings: std::path::PathBuf,
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
            away: false,
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
            transport: transport(&joined.connection),
            name: None,
            speaker: None,
            speaking: Vec::new(),
            chat: Vec::new(),
            reactions: Vec::new(),
            screen_opening: false,
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
            screens: crate::video::Board::default(),
            preview: joined.preview,
        });
        let (roster, followed) = watch::channel(Roster::default());
        let peers = Peers::new(joined.crypto.clone(), joined.path, joined.origin.consume());
        let board = shared.board.clone();
        let voices = crate::speaker::Voices::default();
        let (watching, watched_cameras) = watch::channel(None);
        let (speaker, followed_speaker) = watch::channel(None);
        let (heard, hearing) = mpsc::unbounded_channel();
        let following = joined.runtime.spawn(roster::follow(
            peers,
            joined.crypto,
            shared.invite.room,
            roster,
            roster::Playing {
                mixer: joined.mixer,
                board: board.clone(),
                screens: shared.screens.clone(),
                decoders: joined.decoders.clone(),
                voices: voices.clone(),
                watching: watched_cameras,
                speaker: followed_speaker,
                heard,
                tap: joined.tap.clone(),
            },
        ));
        let task = Task {
            publication: joined.publication,
            profile: joined.profile,
            profile_track: Some(joined.profile_track),
            saying: Some(joined.saying),
            hearing,
            heard_last: std::collections::HashMap::new(),
            reactions_seen: 0,
            connection: Some(joined.connection),
            moq: joined.moq,
            url: joined.url,
            origin: joined.origin,
            following,
            voice: joined.voice,
            camera: joined.camera,
            muted: joined.muted,
            camera_on: joined.camera_on,
            screen: joined.screen.map(Screen::new),
            tap: joined.tap,
            recordings: joined.recordings,
            recording: None,
            sources: crate::recording::Sources {
                board: shared.board.clone(),
                screens: shared.screens.clone(),
                preview: shared.preview.clone(),
            },
            noise: joined.noise,
            roster: followed,
            crypto: shared.crypto.clone(),
            departed_lately: Vec::new(),
            away: Vec::new(),
            away_until: None,
            state,
            events,
            voices,
            watching,
            speaker,
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

    /// This participant's own camera's pictures, as they're sent, before they're encoded: its own
    /// tile shows them. None without a camera that's sent. While the camera is off they keep
    /// coming; a frontend shows its name instead.
    pub fn preview(&self) -> Option<crate::video::Pictures> {
        self.shared.preview.clone()
    }

    /// How many of `whose` camera's pictures were decoded.
    pub fn video_count(&self, whose: ParticipantKey) -> u64 {
        self.shared.board.count(whose)
    }

    /// `whose` screen's pictures, as they're decoded: none until they share one.
    pub fn screen(&self, whose: ParticipantKey) -> crate::video::Pictures {
        self.shared.screens.pictures(whose)
    }

    /// How many of `whose` screen's pictures were decoded.
    pub fn screen_count(&self, whose: ParticipantKey) -> u64 {
        self.shared.screens.count(whose)
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
    /// The chat and reactions tracks, until the room is left.
    saying: Option<crate::chat::Saying>,
    /// What the others say.
    hearing: mpsc::UnboundedReceiver<crate::chat::Heard>,
    /// By broadcast and track, the count of the last thing heard on it: what's heard again is
    /// shown once.
    heard_last: std::collections::HashMap<(String, bool), u64>,
    /// How many reactions were seen.
    reactions_seen: u64,
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
    camera_on: watch::Sender<bool>,
    /// The screenshare, if it can share one.
    screen: Option<Screen>,
    tap: crate::recording::Tap,
    recordings: std::path::PathBuf,
    /// The recording under way, if any.
    recording: Option<crate::recording::Recording>,
    /// What a recording's picture is made of.
    sources: crate::recording::Sources,
    noise: watch::Sender<mic_media::audio::noise::NoiseSuppression>,
    roster: watch::Receiver<Roster>,
    /// The room's crypto: whose path a rejected broadcast is at.
    crypto: RoomCrypto,
    /// Who left lately, and when: lost with the session, if it ends soon after.
    departed_lately: Vec<(std::time::Instant, Participant)>,
    /// Who the room keeps, greyed, through a reconnect, until they're back or `away_until`.
    away: Vec<Participant>,
    away_until: Option<std::time::Instant>,
    state: watch::Sender<RoomState>,
    events: broadcast::Sender<Event>,
    /// When each voice was last loud.
    voices: crate::speaker::Voices,
    /// Whose cameras are shown: all, if none are named.
    watching: watch::Sender<Option<Vec<ParticipantKey>>>,
    /// The active speaker, whose camera's priority is raised.
    speaker: watch::Sender<Option<ParticipantKey>>,
}

impl Task {
    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        let mut roster_open = true;
        let mut looking = tokio::time::interval(crate::speaker::LOOK);
        looking.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    // Answered once the capture opens, which may wait on the desktop's asking.
                    Some(Request::Command(Command::ShareScreen { on: true }, reply)) => {
                        self.share(reply);
                    }
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
                _ = looking.tick() => {
                    self.look_at_speakers();
                    self.give_up_on_away();
                }
                Some(heard) = self.hearing.recv() => self.heard(heard),
                captured = captured(&mut self.screen) => match captured {
                    Captured::Opened(opened) => self.capture_opened(opened),
                    Captured::Ended(count) => self.capture_ended(count),
                },
            }
        }
    }

    /// What someone said into the state and the events, unless it was heard before.
    fn heard(&mut self, heard: crate::chat::Heard) {
        use crate::chat::Said;
        let key = (heard.path, matches!(heard.said, Said::Message { .. }));
        if self
            .heard_last
            .get(&key)
            .is_some_and(|last| heard.n <= *last)
        {
            return;
        }
        self.heard_last.insert(key, heard.n);
        self.said(heard.from, heard.said);
    }

    /// `said` by `from`, into the state and the events.
    fn said(&mut self, from: ParticipantKey, said: crate::chat::Said) {
        use crate::chat::Said;
        let event = match said {
            Said::Message { text } => {
                let name = {
                    let state = self.state.borrow();
                    std::iter::once(&state.me)
                        .chain(&state.participants)
                        .find(|participant| participant.key == from)
                        .map(|participant| participant.profile.name.clone())
                        .unwrap_or_default()
                };
                let message = Message { from, name, text };
                self.state.send_modify(|state| {
                    state.chat.push(message.clone());
                    let over = state.chat.len().saturating_sub(CHAT_KEPT);
                    state.chat.drain(..over);
                });
                Event::Said { message }
            }
            Said::Reaction { reaction } => {
                let reacted = Reacted {
                    seq: self.reactions_seen,
                    from,
                    reaction,
                };
                self.reactions_seen += 1;
                self.state.send_modify(|state| {
                    state.reactions.push(reacted.clone());
                    let over = state.reactions.len().saturating_sub(REACTIONS_KEPT);
                    state.reactions.drain(..over);
                });
                Event::Reacted { reacted }
            }
        };
        let _unheard: Result<usize, _> = self.events.send(event);
    }

    /// The session lost: everyone in the room, and who left just before, kept away, greyed, until
    /// the room is back and they are too.
    fn lost(&mut self) {
        let now = std::time::Instant::now();
        let mut away: Vec<Participant> = self.state.borrow().participants.clone();
        for (when, gone) in self.departed_lately.drain(..) {
            if now.saturating_duration_since(when) < LOST_WITH_IT
                && !away.iter().any(|kept| kept.key == gone.key)
            {
                away.push(gone);
            }
        }
        for kept in &mut away {
            kept.away = true;
        }
        self.away = away.clone();
        self.away_until = None;
        self.state.send_modify(|state| state.participants = away);
    }

    /// Those kept away who aren't back in time, gone.
    fn give_up_on_away(&mut self) {
        if self
            .away_until
            .is_none_or(|until| std::time::Instant::now() < until)
        {
            return;
        }
        self.away_until = None;
        let gone = std::mem::take(&mut self.away);
        if gone.is_empty() {
            return;
        }
        self.state.send_modify(|state| {
            state
                .participants
                .retain(|them| !(them.away && gone.iter().any(|kept| kept.key == them.key)));
        });
        for participant in gone {
            let _unheard: Result<usize, _> = self.events.send(Event::Departed { participant });
        }
    }

    /// Who speaks now, and who the active speaker is, into the state.
    fn look_at_speakers(&self) {
        let now = std::time::Instant::now();
        let (present, last) = {
            let state = self.state.borrow();
            let present: Vec<ParticipantKey> =
                state.participants.iter().map(|other| other.key).collect();
            (present, state.speaker)
        };
        let speaking = self.voices.speaking(&present, now);
        let speaker = self.voices.active(last, &present, now);
        self.speaker.send_if_modified(|shown| {
            let changed = *shown != speaker;
            *shown = speaker;
            changed
        });
        self.state.send_if_modified(|state| {
            let changed = state.speaker != speaker || state.speaking != speaking;
            state.speaker = speaker;
            state.speaking = speaking;
            changed
        });
    }

    async fn command(&mut self, command: Command) -> Result<(), Error> {
        match command {
            Command::Leave => {
                self.leave().await;
                Ok(())
            }
            Command::SetName { name } => self.set_profile(Profile {
                name: Profile::new(&name).name,
                ..self.profile.clone()
            }),
            Command::Say { text } => {
                let text = crate::chat::trimmed(&text);
                if text.is_empty() {
                    return Ok(());
                }
                self.say(crate::chat::Said::Message { text })
            }
            Command::React { reaction } => self.say(crate::chat::Said::Reaction { reaction }),
            Command::SetHand { raised } => self.set_profile(Profile {
                hand: raised,
                ..self.profile.clone()
            }),
            // A share to start is `share`'s, answered once it opens: this stops one, or one
            // still opening.
            Command::ShareScreen { .. } => {
                let screen = self.screen.as_mut().ok_or(Error::NoScreen)?;
                if let Some(opening) = screen.opening.take() {
                    // Nothing was shared, as asked; whoever asked to share hears so.
                    let _unasked = opening.reply.send(Ok(()));
                    self.set_screen_opening(false);
                }
                if let Some(screen) = self.screen.as_mut() {
                    screen.stop();
                }
                self.set_profile(Profile {
                    sharing: false,
                    ..self.profile.clone()
                })
            }
            Command::SetMuted { muted } => {
                self.set_profile(Profile {
                    muted,
                    ..self.profile.clone()
                })?;
                self.muted.send_replace(muted);
                Ok(())
            }
            Command::SetCamera { on } => {
                self.set_profile(Profile {
                    camera_off: !on,
                    ..self.profile.clone()
                })?;
                self.camera_on.send_replace(on);
                Ok(())
            }
            Command::SetNoiseSuppression { mode } => {
                self.noise.send_replace(mode);
                Ok(())
            }
            Command::Watch { participants } => {
                self.watching.send_if_modified(|watching| {
                    let changed = *watching != participants;
                    *watching = participants;
                    changed
                });
                Ok(())
            }
            Command::Record { on } => {
                if on && self.recording.is_none() {
                    let room = self.state.borrow().room.clone();
                    self.recording = Some(
                        crate::recording::Recording::start(
                            &self.tap,
                            &self.recordings,
                            &room,
                            self.state.subscribe(),
                            self.sources.clone(),
                        )
                        .await?,
                    );
                } else if !on && let Some(recording) = self.recording.take() {
                    recording.stop().await;
                }
                self.set_profile(Profile {
                    recording: on,
                    ..self.profile.clone()
                })
            }
        }
    }

    /// `said`, for everyone in the room, and in the state as anyone's is.
    fn say(&mut self, said: crate::chat::Said) -> Result<(), Error> {
        self.saying.as_mut().ok_or(Error::Left)?.say(said.clone())?;
        let me = self.state.borrow().me.key;
        self.said(me, said);
        Ok(())
    }

    /// Starts opening the screen's capture, answering `reply` once it's open, or at once if
    /// there's nothing to share or it's shared or opening already.
    fn share(&mut self, reply: oneshot::Sender<Result<(), Error>>) {
        let Some(screen) = self.screen.as_mut() else {
            let _unasked = reply.send(Err(Error::NoScreen));
            return;
        };
        if screen.open(reply) {
            self.set_screen_opening(true);
        }
    }

    /// A capture opened, or not: the share starts, for everyone, and its asker hears.
    fn capture_opened(&mut self, opened: Result<mic_media::audio::Running, String>) {
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        let Some(opening) = screen.opening.take() else {
            return;
        };
        let started = opened
            .map_err(Error::Screen)
            .and_then(|running| screen.begin(running, opening.capture, &mut self.publication));
        let shared = started.and_then(|()| {
            self.set_profile(Profile {
                sharing: true,
                ..self.profile.clone()
            })
        });
        self.set_screen_opening(false);
        // The asker may have gone; then nobody needs the answer.
        let _unasked = opening.reply.send(shared);
    }

    fn set_screen_opening(&self, opening: bool) {
        self.state.send_if_modified(|state| {
            let changed = state.screen_opening != opening;
            state.screen_opening = opening;
            changed
        });
    }

    /// A capture that ended: if it was the share, the share stops, as if asked to.
    fn capture_ended(&mut self, count: u64) {
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        if !screen.ended_by_itself(count) {
            return;
        }
        tracing::warn!("the screen's capture ended: the share stops");
        screen.stop();
        let profile = Profile {
            sharing: false,
            ..self.profile.clone()
        };
        if let Err(error) = self.set_profile(profile) {
            tracing::warn!(%error, "telling the room the share stopped");
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
        let was = self.state.borrow().connection;
        match (was, connection) {
            (Connection::Connected, Connection::Reconnecting) => self.lost(),
            (Connection::Reconnecting, Connection::Connected) => {
                self.away_until = Some(std::time::Instant::now() + AWAY_FOR);
            }
            _ => {}
        }
        let transport = match connection {
            Connection::Connected => self.connection.as_ref().and_then(transport),
            _ => None,
        };
        self.state.send_if_modified(|state| {
            let changed = state.connection != connection || state.transport != transport;
            state.connection = connection;
            state.transport = transport;
            changed
        });
    }

    /// The new roster into the state, and who came and went, and what didn't stand, as events.
    fn roster_changed(&mut self, roster: Roster) {
        let (before, rejected_before) = {
            let state = self.state.borrow();
            (state.participants.clone(), state.rejected.clone())
        };
        let Roster {
            participants: roster,
            mut rejected,
        } = roster;
        // Whom each poses as: someone in the room (this participant among them) whose path it
        // took, or else whom its catalog names, as a forger with the invite does either.
        let me = self.state.borrow().me.clone();
        for rejected in &mut rejected {
            let segment = rejected
                .path
                .rsplit_once('/')
                .map_or(rejected.path.as_str(), |(segment, _)| segment);
            let present = || std::iter::once(&me).chain(&roster).chain(&before);
            let at_path = present().find(|them| {
                self.crypto
                    .participant_segment(them.key.as_bytes())
                    .is_ok_and(|theirs| theirs == segment)
            });
            // Whom it names stands until whose path it is is known: the room may not have them yet.
            rejected.posing_as = at_path.map(|them| them.key).or(rejected.posing_as);
        }
        let known = |list: &[Participant], participant: &Participant| {
            list.iter().any(|other| other.key == participant.key)
        };
        // Who left, in case the session ends next; who's kept away and is back, no longer away.
        let now = std::time::Instant::now();
        for gone in before
            .iter()
            .filter(|then| !then.away && !known(&roster, then))
        {
            self.departed_lately.push((now, gone.clone()));
        }
        self.departed_lately
            .retain(|(when, _)| now.saturating_duration_since(*when) < LOST_WITH_IT);
        self.away.retain(|kept| !known(&roster, kept));
        let roster: Vec<Participant> = roster
            .into_iter()
            .chain(self.away.iter().cloned())
            .collect();
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
        self.screen = None;
        if let Some(recording) = self.recording.take() {
            recording.stop().await;
        }
        let Some(connection) = self.connection.take() else {
            return;
        };
        if let Some(track) = self.profile_track.take()
            && let Err(error) = track.finish()
        {
            tracing::debug!(%error, "finishing the profile track");
        }
        if let Some(saying) = self.saying.take() {
            saying.finish();
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

/// A participant's screenshare: its track, made at the first share and kept, and what captures
/// it while it's shared.
struct Screen {
    source: crate::video::ScreenSource,
    /// What the capture's pictures are copied into, and the track's sender reads.
    pictures: watch::Sender<Option<Arc<mic_media::video::Picture>>>,
    on: watch::Sender<bool>,
    /// Sends the track, from the first share until the room is left.
    sending: Option<tokio::task::JoinHandle<()>>,
    /// The capture, while it's shared.
    capturing: Option<mic_media::audio::Running>,
    /// Counts the shares started and stopped: a capture that ends is named by the count it
    /// started at, and is the share's end only if nothing started or stopped since.
    count: u64,
    /// The count a capture started at, once it has ended: its sender dropped, as when the screen
    /// went away or the desktop's own "stop sharing" was pressed.
    ended: mpsc::UnboundedSender<u64>,
    ends: mpsc::UnboundedReceiver<u64>,
    /// A capture being opened, and who asked for it.
    opening: Option<Opening>,
}

/// A screen's capture being opened, on a blocking thread: the desktop may ask which screen, and
/// waits for an answer as long as it takes.
struct Opening {
    opened: tokio::task::JoinHandle<Result<mic_media::audio::Running, String>>,
    /// What the capture sends into, once it's open.
    capture: watch::Receiver<Option<Arc<mic_media::video::Picture>>>,
    reply: oneshot::Sender<Result<(), Error>>,
}

impl Screen {
    fn new(source: crate::video::ScreenSource) -> Self {
        let (ended, ends) = mpsc::unbounded_channel();
        Self {
            source,
            pictures: watch::channel(None).0,
            on: watch::channel(false).0,
            sending: None,
            capturing: None,
            count: 0,
            ended,
            ends,
            opening: None,
        }
    }

    /// Starts opening a capture, `reply` answered once it's open (`Task::capture_opened`). If
    /// it's shared or opening already, `reply` is answered now; false then.
    fn open(&mut self, reply: oneshot::Sender<Result<(), Error>>) -> bool {
        if self.capturing.is_some() || self.opening.is_some() {
            let _unasked = reply.send(Ok(()));
            return false;
        }
        // Each capture sends into a channel of its own, copied into `pictures` until it ends.
        let (captured, capture) = watch::channel(None);
        let source = self.source.clone();
        // Dropped unawaited, as when the share is stopped while it opens, what it opened is
        // dropped with it once it's open: the capture stops.
        let opened = tokio::task::spawn_blocking(move || source(captured));
        self.opening = Some(Opening {
            opened,
            capture,
            reply,
        });
        true
    }

    /// Starts sending what an opened capture sends, the track made and named in the catalog the
    /// first time.
    fn begin(
        &mut self,
        running: mic_media::audio::Running,
        mut capture: watch::Receiver<Option<Arc<mic_media::video::Picture>>>,
        publication: &mut Publication,
    ) -> Result<(), Error> {
        self.capturing = Some(running);
        self.count += 1;
        let (count, pictures, ended) = (self.count, self.pictures.clone(), self.ended.clone());
        tokio::spawn(async move {
            while capture.changed().await.is_ok() {
                let picture = capture.borrow_and_update().clone();
                pictures.send_replace(picture);
            }
            // The room may be gone; then nobody asks.
            let _unasked = ended.send(count);
        });
        if self.sending.is_none() {
            let codec = mic_media::video::Codec::Vp9;
            let track = publication.video_track(crate::video::SCREEN, codec)?;
            self.sending = Some(tokio::spawn(crate::video::publish_screen(
                self.pictures.subscribe(),
                track,
                self.on.subscribe(),
            )));
        }
        self.on.send_replace(true);
        Ok(())
    }

    /// Stops capturing; the next share starts with a keyframe.
    fn stop(&mut self) {
        self.on.send_replace(false);
        self.capturing = None;
        self.count += 1;
    }

    /// Whether a capture that ended, at `count`, is the share now: it ended by itself.
    fn ended_by_itself(&self, count: u64) -> bool {
        self.capturing.is_some() && count == self.count
    }
}

/// What became of a screen's capture.
enum Captured {
    /// The capture being opened is open, or not.
    Opened(Result<mic_media::audio::Running, String>),
    /// The capture that started at this count ended.
    Ended(u64),
}

/// The next thing to become of the screen's capture; never, without a screen.
async fn captured(screen: &mut Option<Screen>) -> Captured {
    let Some(Screen { ends, opening, .. }) = screen else {
        return std::future::pending().await;
    };
    tokio::select! {
        // `Screen` keeps a sender, so the channel never closes.
        Some(count) = ends.recv() => Captured::Ended(count),
        opened = async {
            match opening {
                Some(opening) => (&mut opening.opened)
                    .await
                    .map_err(|error| format!("opening the capture: {error}"))?,
                None => std::future::pending().await,
            }
        } => Captured::Opened(opened),
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(sending) = &self.sending {
            sending.abort();
        }
    }
}

/// How `connection` reaches the relay, while it's connected: QUIC counts datagrams, which a
/// WebSocket over TCP has none of (moq-tokio says no more).
fn transport(connection: &moq_tokio::Connection) -> Option<Transport> {
    let stats = connection.monitor().stats()?;
    Some(if stats.packets_sent.is_some() {
        Transport::Quic
    } else {
        Transport::WebSocket
    })
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
