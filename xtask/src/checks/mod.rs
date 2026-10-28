//! `cargo testbox test <phase>`'s checks. Each phase is a file of its own.

mod audio;
mod audio_devices;
mod crashed;
mod e2ee;
mod host;
mod impersonation;
mod links;
mod network;
mod record;
mod recording;
mod relay;
mod room;
mod room_40;
mod roster;
mod screenshare;
mod soak;
mod testbox;
mod version;
mod video;
pub mod window;

use crate::error::{Error, Result};
use crate::podman::TestBox;

pub use self::record::Record;

/// The phases, each a file of its own.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Phase {
    /// The box itself: no egress, no build when nothing changed, the builder, displays, the GPU.
    Box,
    /// Frames pass through the relay between a stock moq publisher and subscriber.
    Relay,
    /// Participants see each other come and go, and rejoin a restarted relay.
    Roster,
    /// A stranger on the relay sees only opaque names and ciphertext.
    E2ee,
    /// A member publishing what it can't sign is rejected.
    Impersonation,
    /// Each hears every other's voice, never their own.
    Audio,
    /// Voices through real devices: cpal, ALSA, PipeWire.
    AudioDevices,
    /// Each decodes every other's camera; one that can't says so.
    Video,
    /// A newer version on the site is said.
    Version,
    /// A mic:// link joins its room.
    Links,
    /// Twelve in a room: the tiles shown, their cameras only, the speaker, chat, reactions, hands.
    Room,
    /// A shared screen reaches everyone; a real one too.
    Screenshare,
    /// Loss, delay, and UDP blocked: what's heard and seen holds, and the WebSocket fallback.
    Network,
    /// One records: everyone sees it, and the files play back.
    Recording,
    /// Forty in a room, measured each minute against the budget: not in a default round.
    #[value(name = "room-40")]
    Room40,
    /// A long call, an hour: still heard and seen, and what each window holds doesn't grow:
    /// not in a default round.
    Soak,
}

impl Phase {
    /// What `run` runs when it isn't told which: every phase but `room-40`, which takes minutes.
    pub const ROUNDS: [Phase; 14] = [
        Phase::Box,
        Phase::Relay,
        Phase::Roster,
        Phase::E2ee,
        Phase::Impersonation,
        Phase::Audio,
        Phase::AudioDevices,
        Phase::Video,
        Phase::Version,
        Phase::Links,
        Phase::Room,
        Phase::Screenshare,
        Phase::Network,
        Phase::Recording,
    ];

    pub fn name(self) -> String {
        clap::ValueEnum::to_possible_value(&self)
            .map(|value| value.get_name().to_owned())
            .unwrap_or_default()
    }

    /// Whether it drives the box's clients: such phases run one at a time, even with `--jobs`,
    /// as two driving alice at once would take each other's rooms. (Leasing clients to phases
    /// lifts that, once there are more phases with clients.)
    pub fn uses_clients(self) -> bool {
        matches!(
            self,
            Phase::Roster
                | Phase::E2ee
                | Phase::Impersonation
                | Phase::Audio
                | Phase::AudioDevices
                | Phase::Video
                | Phase::Links
                | Phase::Room
                | Phase::Screenshare
                | Phase::Network
                | Phase::Recording
                | Phase::Room40
                | Phase::Soak
        )
    }

    /// Whether it runs alone, with no other phase at once: because it disrupts the whole box
    /// (`roster` restarts the relay), or because it measures, and another phase's load would
    /// skew what it measures (`audio`). This is the box-wide measurement lock (PLAN.md, "The test
    /// box").
    pub fn runs_alone(self) -> bool {
        matches!(
            self,
            Phase::Roster
                | Phase::Audio
                | Phase::AudioDevices
                | Phase::Video
                | Phase::Network
                | Phase::Room40
                | Phase::Soak
        )
    }

    pub fn run(self, checks: &mut Checks, the_box: &TestBox) -> Result<()> {
        match self {
            Phase::Box => testbox::testbox(checks, the_box),
            Phase::Relay => relay::relay(checks, the_box),
            Phase::Roster => roster::roster(checks, the_box),
            Phase::E2ee => e2ee::e2ee(checks, the_box),
            Phase::Impersonation => impersonation::impersonation(checks, the_box),
            Phase::Audio => audio::audio(checks, the_box),
            Phase::AudioDevices => audio_devices::audio_devices(checks, the_box),
            Phase::Video => video::video(checks, the_box),
            Phase::Version => version::version(checks, the_box),
            Phase::Links => links::links(checks, the_box),
            Phase::Room => room::room(checks, the_box),
            Phase::Screenshare => screenshare::screenshare(checks, the_box),
            Phase::Network => network::network(checks, the_box),
            Phase::Room40 => room_40::room_40(checks, the_box),
            Phase::Recording => recording::recording(checks, the_box),
            Phase::Soak => soak::soak(checks, the_box),
        }
    }
}

/// Runs a phase's checks on a fresh tally, then fails if any of them failed. Last, also after a
/// check the rest depend on failed: that no container crashed ([`crashed`]); then the logs go
/// into the phase's [`Record`].
pub fn phase(
    the_box: &TestBox,
    record: Record,
    prefix: &str,
    run: impl FnOnce(&mut Checks) -> Result<()>,
) -> Result<()> {
    let mut checks = Checks {
        the_box,
        failed: Vec::new(),
        kept: record,
        prefix: prefix.to_owned(),
    };
    let ran = run(&mut checks);
    checks.record("no container crashed", || crashed::none(the_box));
    checks.kept.keep_logs(the_box);
    println!(
        "{}record: {}",
        checks.prefix,
        checks.kept.directory().display()
    );
    ran?;
    checks.finish()
}

pub struct Checks<'a> {
    the_box: &'a TestBox,
    failed: Vec<String>,
    kept: Record,
    /// What each line of output starts with: the phase's name, when phases run at once.
    prefix: String,
}

impl Checks<'_> {
    /// A check later checks depend on: its failure ends the run (the caller's `?`).
    pub fn require<Found>(
        &mut self,
        name: &str,
        check: impl FnOnce() -> Result<Found>,
    ) -> Result<Found> {
        self.report(name, check())
            .ok_or_else(|| Error::Failed(format!("{name} failed")))
    }

    /// A check nothing depends on: a failure is tallied for [`Self::finish`] and the run goes on.
    pub fn record(&mut self, name: &str, check: impl FnOnce() -> Result<()>) {
        self.report(name, check());
    }

    /// A check that doesn't apply to this box, and why: said, not tallied.
    pub fn skip(&mut self, name: &str, why: &str) {
        println!("{}check {name}: skipped: {why}", self.prefix);
    }

    /// What takes each display during a check, for what shows only for a moment
    /// (`--screenshots`): taken before the checks, used in them.
    pub fn moments(&self) -> record::Moments {
        self.kept.moments()
    }

    /// A check that measures (a frame rate, CPU, a voice under loss), with what others did on
    /// the host meanwhile: said, and if they loaded it, a failure is said to be unreliable and
    /// isn't tallied.
    pub fn measured(&mut self, name: &str, check: impl FnOnce() -> Result<()>) {
        let host = host::Host::now(self.the_box);
        let outcome = check();
        let others = host.since();
        println!("{}check {name}: {others}", self.prefix);
        match outcome {
            Err(reason) if others.busy() => {
                self.kept.checked(self.the_box, name, true);
                println!(
                    "{}check {name}: unreliable, the host was busy: {reason}",
                    self.prefix
                );
            }
            outcome => {
                self.report(name, outcome);
            }
        }
    }

    /// Where the phase's record is, for a check that keeps a file there.
    pub fn record_directory(&self) -> &std::path::Path {
        self.kept.directory()
    }

    /// Prints the outcome; a failure is tallied and yields `None`.
    fn report<Found>(&mut self, name: &str, outcome: Result<Found>) -> Option<Found> {
        self.kept.checked(self.the_box, name, outcome.is_err());
        match outcome {
            Ok(found) => {
                println!("{}check {name}: passed", self.prefix);
                Some(found)
            }
            Err(reason) => {
                println!("{}check {name}: failed: {reason}", self.prefix);
                self.failed.push(name.to_owned());
                None
            }
        }
    }

    fn finish(self) -> Result<()> {
        if self.failed.is_empty() {
            Ok(())
        } else {
            Err(Error::Failed(format!("failed: {}", self.failed.join(", "))))
        }
    }
}

/// A PNG's pixels, and how many samples each has.
pub fn png_pixels(path: &std::path::Path) -> Result<(Vec<u8>, usize)> {
    let file = std::fs::File::open(path).map_err(|source| Error::Io {
        what: path.display().to_string(),
        source,
    })?;
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .map_err(|error| Error::Failed(format!("{}: {error}", path.display())))?;
    let mut pixels = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|error| Error::Failed(format!("{}: {error}", path.display())))?;
    pixels.truncate(info.buffer_size());
    Ok((pixels, info.color_type.samples()))
}
