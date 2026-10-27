//! Checkpoint signatures (D18, step 2, §5.3.2): the publisher signs, every 250 ms, how far each of
//! its tracks has got, on a `sig` track of its own; subscribers show frames as they come and
//! confirm each against the first checkpoint that covers it. A frame a checkpoint contradicts,
//! or one no checkpoint covers within the 2 s grace, rejects the broadcast: it fails closed.
//!
//! The catalog proves whose a broadcast is; checkpoints prove the frames are theirs, which a
//! member with the room's secret could otherwise forge, answering at someone's path (§5.3.2, P6)
//! with their replayed catalog.
//!
//! Each group of each track has a hash chain over its frames:
//!
//! ```text
//! h0 = SHA-256("mic/v1/frames\0" track:str group:u64)
//! h(n+1) = SHA-256(h(n) length:u64 plaintext)
//! ```
//!
//! A checkpoint is canonical bytes, signed with the participant's key in the room:
//!
//! ```text
//! "mic/v1/checkpoint\0" format:u32 room:16 kid:u64 epoch:str seq:u64
//!                       entries:u16 (track:str group:u64 frames:u64 hash:32)* signature:64
//! ```
//!
//! It lists, for each track, its latest group, and every group that changed in the last 3 s, so
//! a group a newer one superseded is still covered. One is sent within 250 ms of a frame, and once
//! a second all the same, so frames written before a subscriber came are covered too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ed25519_dalek::{Signature, Signer as _, Verifier as _, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::invite::RoomId;
use crate::keys::RoomKey;
use crate::roster::ParticipantKey;
use crate::tracks::TrackError;

/// The checkpoints' track's semantic name.
pub const SIG: &str = "sig";
/// How often a publisher checkpoints what changed.
pub const INTERVAL: Duration = Duration::from_millis(250);
/// How long a frame may go without a checkpoint covering it before the broadcast is rejected.
pub const GRACE: Duration = Duration::from_secs(2);
/// How often a checkpoint is sent though nothing changed.
const HEARTBEAT: Duration = Duration::from_secs(1);
/// How long a group stays in checkpoints after it last changed.
const RECENT: Duration = Duration::from_secs(3);
const FRAMES: &[u8] = b"mic/v1/frames\0";
const DOMAIN: &[u8] = b"mic/v1/checkpoint\0";
const FORMAT: u32 = 1;
const SIGNATURE: usize = 64;

/// A hash chain's link: where a group's frames have got.
pub type Link = [u8; 32];

/// A group's chain before its first frame.
pub fn chain_start(track: &str, group: u64) -> Link {
    let mut hasher = Sha256::new();
    hasher.update(FRAMES);
    hasher.update((track.len() as u64).to_be_bytes());
    hasher.update(track.as_bytes());
    hasher.update(group.to_be_bytes());
    hasher.finalize().into()
}

/// The chain after `plaintext`.
pub fn chain_next(link: &Link, plaintext: &[u8]) -> Link {
    let mut hasher = Sha256::new();
    hasher.update(link);
    hasher.update((plaintext.len() as u64).to_be_bytes());
    hasher.update(plaintext);
    hasher.finalize().into()
}

/// How far one group of one track has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub track: String,
    pub group: u64,
    /// Frames so far.
    pub frames: u64,
    /// The chain after them.
    pub hash: Link,
}

/// A publisher's word on how far its tracks have got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub room: RoomId,
    pub kid: u64,
    pub epoch: String,
    /// Up by one with every checkpoint of the broadcast.
    pub seq: u64,
    pub entries: Vec<Entry>,
}

/// Why a broadcast's frames don't stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forgery {
    /// A checkpoint that doesn't read, or whose signature doesn't check.
    Checkpoint(String),
    /// A frame its checkpoint contradicts.
    Contradicted { track: String, group: u64 },
    /// A frame no checkpoint covered within the grace.
    Unconfirmed { track: String, group: u64 },
}

impl std::fmt::Display for Forgery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Checkpoint(why) => write!(f, "a checkpoint that doesn't stand: {why}"),
            Self::Contradicted { track, group } => write!(
                f,
                "frames its signed checkpoint contradicts ({track}, group {group})"
            ),
            Self::Unconfirmed { track, group } => write!(
                f,
                "frames no signed checkpoint covered within {}s ({track}, group {group})",
                GRACE.as_secs()
            ),
        }
    }
}

impl Checkpoint {
    pub fn sign(&self, key: &RoomKey) -> Vec<u8> {
        let mut bytes = self.encode();
        bytes.extend_from_slice(&key.signing().sign(&bytes).to_bytes());
        bytes
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend_from_slice(&FORMAT.to_be_bytes());
        bytes.extend_from_slice(self.room.as_bytes());
        bytes.extend_from_slice(&self.kid.to_be_bytes());
        put_str(&mut bytes, &self.epoch);
        bytes.extend_from_slice(&self.seq.to_be_bytes());
        let count = u16::try_from(self.entries.len()).expect("fewer than 65536 entries");
        bytes.extend_from_slice(&count.to_be_bytes());
        for entry in &self.entries {
            put_str(&mut bytes, &entry.track);
            bytes.extend_from_slice(&entry.group.to_be_bytes());
            bytes.extend_from_slice(&entry.frames.to_be_bytes());
            bytes.extend_from_slice(&entry.hash);
        }
        bytes
    }

    /// The checkpoint `bytes` are, if `key` signed it for this room, key id and instance.
    pub fn verify(
        bytes: &[u8],
        key: &ParticipantKey,
        room: RoomId,
        kid: u64,
        epoch: &str,
    ) -> Result<Self, String> {
        if bytes.len() < DOMAIN.len() + SIGNATURE {
            return Err("too short".to_owned());
        }
        let (signed, signature) = bytes.split_at(bytes.len() - SIGNATURE);
        let verifying = VerifyingKey::from_bytes(key.as_bytes()).map_err(|_| "not a key")?;
        let signature = Signature::from_slice(signature).map_err(|_| "not a signature")?;
        verifying
            .verify(signed, &signature)
            .map_err(|_| "its signature doesn't check".to_owned())?;
        let mut reader = Reader(signed);
        if reader.take(DOMAIN.len())? != DOMAIN {
            return Err("not a checkpoint".to_owned());
        }
        if u32::from_be_bytes(reader.array()?) != FORMAT {
            return Err("of another format".to_owned());
        }
        let checkpoint = Self {
            room: RoomId::from_bytes(reader.array()?),
            kid: u64::from_be_bytes(reader.array()?),
            epoch: reader.str()?,
            seq: u64::from_be_bytes(reader.array()?),
            entries: {
                let count = u16::from_be_bytes(reader.array()?);
                (0..count)
                    .map(|_| {
                        Ok(Entry {
                            track: reader.str()?,
                            group: u64::from_be_bytes(reader.array()?),
                            frames: u64::from_be_bytes(reader.array()?),
                            hash: reader.array()?,
                        })
                    })
                    .collect::<Result<_, String>>()?
            },
        };
        if !reader.0.is_empty() {
            return Err("bytes after its end".to_owned());
        }
        if checkpoint.room != room || checkpoint.kid != kid || checkpoint.epoch != epoch {
            return Err("of another room, key id or publisher instance".to_owned());
        }
        Ok(checkpoint)
    }
}

fn put_str(bytes: &mut Vec<u8>, text: &str) {
    let length = u16::try_from(text.len()).expect("a checkpoint's text is shorter than 64 KiB");
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        if self.0.len() < count {
            return Err("cut short".to_owned());
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("taken N bytes"))
    }

    fn str(&mut self) -> Result<String, String> {
        let length = u16::from_be_bytes(self.array()?);
        let text = self.take(usize::from(length))?;
        String::from_utf8(text.to_vec()).map_err(|_| "text that isn't UTF-8".to_owned())
    }
}

// ── Publishing ──────────────────────────────────────────────────────

/// What a publisher's tracks have written, for its checkpoints: shared by its signed tracks and
/// its signing task.
#[derive(Clone, Default)]
pub struct Ledger(Arc<Mutex<LedgerState>>);

#[derive(Default)]
struct LedgerState {
    /// By track and group: how far it got, and when it last changed.
    groups: HashMap<(String, u64), (Entry, Instant)>,
    /// Whether anything changed since the last checkpoint.
    changed: bool,
}

impl Ledger {
    fn record(&self, track: &str, group: u64, plaintext: &[u8]) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (entry, changed) = state
            .groups
            .entry((track.to_owned(), group))
            .or_insert_with(|| {
                (
                    Entry {
                        track: track.to_owned(),
                        group,
                        frames: 0,
                        hash: chain_start(track, group),
                    },
                    Instant::now(),
                )
            });
        entry.hash = chain_next(&entry.hash, plaintext);
        entry.frames += 1;
        *changed = Instant::now();
        state.changed = true;
    }

    /// The entries a checkpoint now lists: each track's latest group, and every group that
    /// changed within [`RECENT`]; and whether anything changed since the last call.
    fn take(&self) -> (Vec<Entry>, bool) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = std::mem::take(&mut state.changed);
        let mut latest: HashMap<&str, u64> = HashMap::new();
        for (track, group) in state.groups.keys() {
            let newest = latest.entry(track).or_insert(*group);
            *newest = (*newest).max(*group);
        }
        let now = Instant::now();
        let mut entries: Vec<Entry> = state
            .groups
            .values()
            .filter(|(entry, when)| {
                latest.get(entry.track.as_str()) == Some(&entry.group) || now - *when < RECENT
            })
            .map(|(entry, _)| entry.clone())
            .collect();
        entries.sort_by(|left, right| (&left.track, left.group).cmp(&(&right.track, right.group)));
        // Forget groups that will never be listed again.
        let latest: HashMap<String, u64> = latest
            .into_iter()
            .map(|(track, group)| (track.to_owned(), group))
            .collect();
        state.groups.retain(|(track, group), (_, when)| {
            latest.get(track) == Some(group) || now - *when < RECENT
        });
        (entries, changed)
    }
}

/// A track whose frames the publisher's checkpoints cover.
pub struct SignedTrack {
    semantic: String,
    track: moq_e2ee::track::Producer,
    ledger: Ledger,
}

impl SignedTrack {
    pub(crate) fn new(semantic: &str, track: moq_e2ee::track::Producer, ledger: Ledger) -> Self {
        Self {
            semantic: semantic.to_owned(),
            track,
            ledger,
        }
    }

    /// A new group, after the last.
    pub fn append_group(&mut self) -> Result<SignedGroup, TrackError> {
        let group = self.track.append_group()?;
        Ok(SignedGroup {
            sequence: group.sequence(),
            group,
            semantic: self.semantic.clone(),
            ledger: self.ledger.clone(),
        })
    }

    pub fn finish(self) -> Result<(), TrackError> {
        Ok(self.track.finish()?)
    }
}

/// A group of a signed track.
pub struct SignedGroup {
    group: moq_e2ee::group::Producer,
    sequence: u64,
    semantic: String,
    ledger: Ledger,
}

impl SignedGroup {
    pub fn write_frame(
        &mut self,
        timestamp: moq_net::Timestamp,
        plaintext: &[u8],
    ) -> Result<(), TrackError> {
        self.group.write_frame(timestamp, plaintext)?;
        self.ledger.record(&self.semantic, self.sequence, plaintext);
        Ok(())
    }

    pub fn finish(self) -> Result<(), TrackError> {
        Ok(self.group.finish()?)
    }
}

/// Signs a checkpoint into `track` within [`INTERVAL`] of a change, and every [`HEARTBEAT`] all
/// the same, for as long as it runs.
pub(crate) async fn sign(
    ledger: Ledger,
    mut track: moq_e2ee::track::Producer,
    key: RoomKey,
    base: Checkpoint,
) {
    let mut seq = base.seq;
    let mut since_sent = HEARTBEAT;
    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        since_sent += INTERVAL;
        let (entries, changed) = ledger.take();
        if entries.is_empty() || (!changed && since_sent < HEARTBEAT) {
            continue;
        }
        seq += 1;
        let checkpoint = Checkpoint {
            seq,
            entries,
            ..base.clone()
        };
        let written = track.append_group().and_then(|mut group| {
            group.write_frame(moq_net::Timestamp::now(), &checkpoint.sign(&key))?;
            group.finish()
        });
        if let Err(error) = written {
            tracing::debug!(%error, "a checkpoint wasn't written");
            return;
        }
        since_sent = Duration::ZERO;
    }
}

// ── Verifying ───────────────────────────────────────────────────────

/// What a subscriber knows of one peer's frames: what its checkpoints confirmed, and what it
/// read that no checkpoint has yet. Shared by the peer's verified tracks and its checking task.
#[derive(Clone, Default)]
pub struct Verification(Arc<Mutex<VerificationState>>);

#[derive(Default)]
struct VerificationState {
    /// By track and group: the chain at every frame read, from the first, and the frames before
    /// them, if the group was joined late.
    read: HashMap<(String, u64), Group>,
    /// By track and group: the furthest a checkpoint said it got.
    confirmed: HashMap<(String, u64), (u64, Link)>,
    /// The last checkpoint's number.
    last: Option<u64>,
    failed: Option<Forgery>,
    /// Tracks the subscriber let go of mid-group, whose frames read since are ignored until it
    /// subscribes again.
    abandoned: std::collections::HashSet<String>,
}

/// One group as a subscriber read it.
struct Group {
    /// The chain after each frame read, in order: links[i] is after frame `first + i`.
    links: Vec<(Link, Instant)>,
    /// How many frames were confirmed already.
    confirmed: usize,
}

impl Verification {
    /// A frame of `group` read, after those read before it: a group reaches a subscriber from
    /// its start, as one stream.
    fn read(&self, track: &str, group: u64, plaintext: &[u8]) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.abandoned.contains(track) {
            return;
        }
        let key = (track.to_owned(), group);
        let read = state.read.entry(key.clone()).or_insert_with(|| Group {
            links: Vec::new(),
            confirmed: 0,
        });
        let previous = read
            .links
            .last()
            .map_or_else(|| chain_start(track, group), |(link, _)| *link);
        read.links
            .push((chain_next(&previous, plaintext), Instant::now()));
        if let Some((frames, hash)) = state.confirmed.get(&key).copied() {
            state.confirm(&key, frames, &hash);
        }
    }

    /// A checkpoint's entries: each confirms, or contradicts, what was read of its group.
    fn checkpoint(&self, checkpoint: Checkpoint) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.last.is_some_and(|last| checkpoint.seq <= last) {
            return;
        }
        state.last = Some(checkpoint.seq);
        for entry in checkpoint.entries {
            let key = (entry.track, entry.group);
            state
                .confirmed
                .insert(key.clone(), (entry.frames, entry.hash));
            state.confirm(&key, entry.frames, &entry.hash);
        }
    }

    /// The broadcast's forgery, if one was found, frames left unconfirmed past the grace
    /// counting as one.
    fn check(&self) -> Option<Forgery> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.failed.is_none() {
            let now = Instant::now();
            let late = state.read.iter().find(|(_, group)| {
                group.links[group.confirmed..]
                    .first()
                    .is_some_and(|(_, when)| now - *when > GRACE)
            });
            if let Some(((track, group), _)) = late {
                state.failed = Some(Forgery::Unconfirmed {
                    track: track.clone(),
                    group: *group,
                });
            }
        }
        state.failed.clone()
    }

    /// `track` let go of by the subscriber, as a camera no longer shown is: the frames of it
    /// read and not confirmed are forgotten, since the rest of their groups, which the
    /// checkpoints confirm them with, won't be read; and so are any it reads until it's
    /// subscribed to again ([`Self::resume`]). What was read is read again from its group's start
    /// then.
    pub(crate) fn abandon(&self, track: &str) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.read.retain(|(read, _), _| read != track);
        state.abandoned.insert(track.to_owned());
    }

    /// `track` subscribed to again, after [`Self::abandon`].
    fn resume(&self, track: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abandoned
            .remove(track);
    }

    fn fail(&self, forgery: Forgery) {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.failed.get_or_insert(forgery);
    }
}

impl VerificationState {
    /// The group `key` as a checkpoint says it got: `frames` frames, the chain `hash` after them.
    fn confirm(&mut self, key: &(String, u64), frames: u64, hash: &Link) {
        let Some(read) = self.read.get_mut(key) else {
            return;
        };
        let Ok(frames) = usize::try_from(frames) else {
            return;
        };
        // Not read that far yet: the frames will be confirmed as they come.
        if frames == 0 || frames > read.links.len() {
            return;
        }
        if read.links[frames - 1].0 == *hash {
            read.confirmed = read.confirmed.max(frames);
        } else {
            self.failed.get_or_insert(Forgery::Contradicted {
                track: key.0.clone(),
                group: key.1,
            });
        }
    }
}

/// A peer's signed track, its frames recorded for the checkpoints to confirm.
pub struct VerifiedTrack {
    semantic: String,
    track: moq_e2ee::track::Consumer,
    verification: Verification,
}

impl VerifiedTrack {
    pub(crate) fn new(
        semantic: &str,
        track: moq_e2ee::track::Consumer,
        verification: Verification,
    ) -> Self {
        verification.resume(semantic);
        Self {
            semantic: semantic.to_owned(),
            track,
            verification,
        }
    }

    /// The next group, whose frames are confirmed as they're read.
    pub async fn recv_group(&mut self) -> Result<Option<VerifiedGroup>, TrackError> {
        let Some(group) = self.track.recv_group().await? else {
            return Ok(None);
        };
        Ok(Some(VerifiedGroup {
            sequence: group.sequence(),
            group,
            semantic: self.semantic.clone(),
            verification: self.verification.clone(),
        }))
    }
}

/// A group of a peer's signed track.
pub struct VerifiedGroup {
    group: moq_e2ee::group::Consumer,
    sequence: u64,
    semantic: String,
    verification: Verification,
}

impl VerifiedGroup {
    /// The next frame, shown at once and confirmed by a checkpoint within the grace, or the
    /// broadcast is rejected.
    pub async fn read_frame(&mut self) -> Result<Option<moq_e2ee::group::Frame>, TrackError> {
        let frame = self.group.read_frame().await?;
        if let Some(frame) = &frame {
            self.verification
                .read(&self.semantic, self.sequence, &frame.plaintext);
        }
        Ok(frame)
    }
}

/// Reads a peer's checkpoints into `verification` and checks its frames against them, until it
/// finds a forgery, which it returns. The peer's checkpoints are verified with `key`, which its
/// catalog stands for.
pub(crate) async fn verify(
    mut checkpoints: moq_e2ee::track::Consumer,
    verification: Verification,
    key: ParticipantKey,
    room: RoomId,
    kid: u64,
    epoch: String,
) -> Forgery {
    let mut tick = tokio::time::interval(INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ended = false;
    loop {
        tokio::select! {
            group = checkpoints.recv_group(), if !ended => match group {
                Ok(Some(mut group)) => {
                    let Ok(Some(frame)) = group.read_frame().await else {
                        continue;
                    };
                    match Checkpoint::verify(&frame.plaintext, &key, room, kid, &epoch) {
                        Ok(checkpoint) => verification.checkpoint(checkpoint),
                        Err(why) => verification.fail(Forgery::Checkpoint(why)),
                    }
                }
                // Its checkpoints ended: frames read from now on will go unconfirmed.
                Ok(None) => ended = true,
                Err(error) => verification.fail(Forgery::Checkpoint(error.to_string())),
            },
            _ = tick.tick() => {}
        }
        if let Some(forgery) = verification.check() {
            return forgery;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::DeviceSecret;

    const EPOCH: &str = "01a17a43-1786-730f-a3b3-15083bc6f37b";

    fn key(device: u8) -> RoomKey {
        DeviceSecret::from_bytes([device; 32]).room_key(&RoomId::from_bytes([1; 16]))
    }

    fn participant(key: &RoomKey) -> ParticipantKey {
        ParticipantKey::from_bytes(key.public().to_bytes())
    }

    fn checkpoint(seq: u64, entries: Vec<Entry>) -> Checkpoint {
        Checkpoint {
            room: RoomId::from_bytes([1; 16]),
            kid: 0,
            epoch: EPOCH.to_owned(),
            seq,
            entries,
        }
    }

    /// The entry for `frames` of group 0 of `profile`.
    fn entry(frames: &[&[u8]]) -> Entry {
        let hash = frames
            .iter()
            .fold(chain_start("profile", 0), |link, frame| {
                chain_next(&link, frame)
            });
        Entry {
            track: "profile".to_owned(),
            group: 0,
            frames: frames.len() as u64,
            hash,
        }
    }

    #[test]
    fn a_checkpoint_verifies_with_its_signers_key_only() {
        let alice = key(1);
        let signed = checkpoint(1, vec![entry(&[b"one"])]).sign(&alice);
        let room = RoomId::from_bytes([1; 16]);
        assert_eq!(
            Checkpoint::verify(&signed, &participant(&alice), room, 0, EPOCH),
            Ok(checkpoint(1, vec![entry(&[b"one"])]))
        );
        assert!(Checkpoint::verify(&signed, &participant(&key(2)), room, 0, EPOCH).is_err());
        assert!(Checkpoint::verify(&signed, &participant(&alice), room, 0, "another").is_err());
        let mut tampered = signed.clone();
        tampered[DOMAIN.len() + 10] ^= 1;
        assert!(Checkpoint::verify(&tampered, &participant(&alice), room, 0, EPOCH).is_err());
    }

    #[test]
    fn frames_a_checkpoint_covers_are_confirmed() {
        let verification = Verification::default();
        verification.read("profile", 0, b"one");
        verification.read("profile", 0, b"two");
        verification.checkpoint(checkpoint(1, vec![entry(&[b"one", b"two"])]));
        assert_eq!(verification.check(), None);
        let state = verification.0.lock().unwrap();
        assert_eq!(state.read[&("profile".to_owned(), 0)].confirmed, 2);
    }

    #[test]
    fn a_checkpoint_that_comes_first_confirms_the_frames_after_it() {
        let verification = Verification::default();
        verification.checkpoint(checkpoint(1, vec![entry(&[b"one"])]));
        verification.read("profile", 0, b"one");
        assert_eq!(verification.check(), None);
        assert_eq!(
            verification.0.lock().unwrap().read[&("profile".to_owned(), 0)].confirmed,
            1
        );
    }

    #[test]
    fn a_forged_frame_is_contradicted() {
        let verification = Verification::default();
        verification.read("profile", 0, b"forged");
        verification.checkpoint(checkpoint(1, vec![entry(&[b"real"])]));
        assert_eq!(
            verification.check(),
            Some(Forgery::Contradicted {
                track: "profile".to_owned(),
                group: 0
            })
        );
    }

    #[test]
    fn a_frame_no_checkpoint_covers_fails_after_the_grace() {
        let verification = Verification::default();
        verification.read("profile", 0, b"one");
        assert_eq!(verification.check(), None);
        // As if the grace had passed.
        verification
            .0
            .lock()
            .unwrap()
            .read
            .get_mut(&("profile".to_owned(), 0))
            .unwrap()
            .links[0]
            .1 -= GRACE + Duration::from_millis(1);
        assert_eq!(
            verification.check(),
            Some(Forgery::Unconfirmed {
                track: "profile".to_owned(),
                group: 0
            })
        );
    }

    #[test]
    fn an_old_checkpoint_again_changes_nothing() {
        let verification = Verification::default();
        verification.read("profile", 0, b"one");
        verification.checkpoint(checkpoint(2, vec![entry(&[b"one"])]));
        // A replayed older one, with other frames: ignored, not a contradiction.
        verification.checkpoint(checkpoint(1, vec![entry(&[b"other"])]));
        assert_eq!(verification.check(), None);
    }

    #[test]
    fn the_ledger_lists_latest_and_recent_groups() {
        let ledger = Ledger::default();
        ledger.record("profile", 0, b"one");
        ledger.record("profile", 1, b"two");
        let (entries, changed) = ledger.take();
        assert!(changed);
        // Both: group 0 changed just now, and group 1 is the latest.
        assert_eq!(entries.len(), 2);
        let (again, changed) = ledger.take();
        assert!(!changed);
        assert_eq!(again, entries);
    }
}
