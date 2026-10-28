//! What a proof earns: the decision, and what it remembers to make it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use derive_more::{Display, Error};
use moq_auth::{Event, Grant, Pattern, Patterns, Request};

use crate::proof::{PARAM, Proof, ProofError};

/// The limits of one auth server. Memory is the only state, so a restart forgets who came first.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Policy {
    /// How long a room, or a path in it, is remembered after it was last asked for.
    pub memory: Duration,
    /// The most rooms remembered at once: past it, a new room is refused until one is forgotten.
    pub rooms: usize,
    /// The most paths remembered in a room.
    pub participants: usize,
    /// How often the relay asks again for a session: a grant that's no longer given (the server
    /// restarted, or the room is full) ends it.
    pub revalidate: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            memory: Duration::from_secs(24 * 60 * 60),
            rooms: 100_000,
            // §5.8: rooms of up to 30–40.
            participants: 256,
            revalidate: Duration::from_secs(5 * 60),
        }
    }
}

impl Policy {
    /// This policy with the relay asking again for each session every `every`.
    pub fn with_revalidate(mut self, every: Duration) -> Self {
        self.revalidate = every;
        self
    }
}

/// Why a session isn't let in; the body of the 403, which the relay logs and the client never
/// sees.
#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum Refusal {
    #[display("no proof: the query has no `{PARAM}`")]
    NoProof,
    #[display("a proof that doesn't hold: {_0}")]
    Proof(ProofError),
    #[display("the proof is for another room than the one dialed")]
    WrongRoom,
    #[display("the room is held by another key")]
    RoomKey,
    #[display("the path is held by another participant")]
    PathTaken,
    #[display("too many rooms, or too many in the room")]
    Full,
}

struct Room {
    key: [u8; 32],
    /// Path to its participant's key.
    paths: HashMap<String, [u8; 32]>,
    seen: Instant,
}

#[derive(Default)]
struct Rooms {
    by_prefix: HashMap<String, Room>,
    swept: Option<Instant>,
}

/// The decision on a session's request.
pub struct Admission {
    policy: Policy,
    rooms: Mutex<Rooms>,
}

impl Admission {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            rooms: Mutex::default(),
        }
    }

    /// The grant for `request`, or why not; `None` for an `end`, which asks for nothing.
    ///
    /// A `revalidate` is decided as a `connect` is: the proof is the same, so the room and the
    /// path are the ones the session was let in for, or, after a restart, are fixed anew.
    pub fn answer(&self, request: &Request) -> Result<Option<Grant>, Refusal> {
        self.answer_at(request, Instant::now())
    }

    fn answer_at(&self, request: &Request, now: Instant) -> Result<Option<Grant>, Refusal> {
        if matches!(request.event, Event::End { .. }) {
            return Ok(None);
        }
        let proof = proof_of(request)?;
        if request.path.trim_matches('/') != proof.prefix() {
            return Err(Refusal::WrongRoom);
        }
        self.admit(&proof, now)?;
        let mut grant = Grant::new(pattern(&format!("{}/**", proof.segment())), pattern("**"));
        grant.expires = SystemTime::now().checked_add(self.policy.revalidate * 2);
        grant.revalidate = Some(self.policy.revalidate);
        Ok(Some(grant))
    }

    fn admit(&self, proof: &Proof, now: Instant) -> Result<(), Refusal> {
        let mut rooms = self.rooms.lock().expect("no panic holds the lock");
        rooms.sweep(self.policy.memory, now);
        let full = rooms.by_prefix.len() >= self.policy.rooms;
        let room = match rooms.by_prefix.get_mut(proof.prefix()) {
            Some(room) => room,
            None if full => return Err(Refusal::Full),
            None => rooms
                .by_prefix
                .entry(proof.prefix().to_owned())
                .or_insert(Room {
                    key: *proof.room(),
                    paths: HashMap::new(),
                    seen: now,
                }),
        };
        if room.key != *proof.room() {
            return Err(Refusal::RoomKey);
        }
        match room.paths.get(proof.segment()) {
            Some(held) if held != proof.participant() => return Err(Refusal::PathTaken),
            Some(_) => {}
            None if room.paths.len() >= self.policy.participants => return Err(Refusal::Full),
            None => {
                room.paths
                    .insert(proof.segment().to_owned(), *proof.participant());
            }
        }
        room.seen = now;
        Ok(())
    }
}

impl Rooms {
    /// Forgets rooms not asked for in `memory`, at most once a minute.
    fn sweep(&mut self, memory: Duration, now: Instant) {
        if self
            .swept
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(60))
        {
            return;
        }
        self.swept = Some(now);
        self.by_prefix
            .retain(|_, room| now.saturating_duration_since(room.seen) < memory);
    }
}

fn proof_of(request: &Request) -> Result<Proof, Refusal> {
    let query = request.query.as_deref().ok_or(Refusal::NoProof)?;
    // The last one wins, as a `jwt` does on the relay: a client that appends is believed.
    let text = query
        .split('&')
        .filter_map(|pair| pair.strip_prefix(PARAM)?.strip_prefix('='))
        .rfind(|text| !text.is_empty())
        .ok_or(Refusal::NoProof)?;
    Proof::decode(text).map_err(Refusal::Proof)
}

fn pattern(text: &str) -> Patterns {
    let pattern: Pattern = text
        .parse()
        .expect("a checked path and `/**` are a pattern");
    [pattern].into_iter().collect()
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use moq_auth::Transport;

    use super::*;

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn request(path: &str, proof: Option<&Proof>) -> Request {
        let mut request = Request::new("test", Transport::Quic, path);
        request.query = proof.map(|proof| format!("{PARAM}={}", proof.encode()));
        request
    }

    /// `participant`'s proof in `room`'s room `prefix`, at `segment`.
    fn proof(prefix: &str, segment: &str, room: u8, participant: u8) -> Proof {
        Proof::sign(prefix, segment, &key(room), &key(participant)).unwrap()
    }

    fn admission() -> Admission {
        Admission::new(Policy::default())
    }

    #[test]
    fn a_member_may_subscribe_to_the_room_and_publish_at_its_path() {
        let grant = admission()
            .answer(&request("/room", Some(&proof("room", "alice", 1, 2))))
            .unwrap()
            .unwrap();
        assert_eq!(grant.publish, pattern("alice/**"));
        assert_eq!(grant.subscribe, pattern("**"));
        assert!(grant.revalidate.is_some() && grant.expires.is_some());
        grant.validate().unwrap();
    }

    #[test]
    fn no_proof_is_no_entry() {
        assert_eq!(
            admission().answer(&request("/room", None)).unwrap_err(),
            Refusal::NoProof
        );
    }

    #[test]
    fn a_proof_is_for_the_room_it_names() {
        let proof = proof("room", "alice", 1, 2);
        assert_eq!(
            admission()
                .answer(&request("/other", Some(&proof)))
                .unwrap_err(),
            Refusal::WrongRoom
        );
    }

    #[test]
    fn a_stranger_cannot_enter_a_room_that_exists() {
        let admission = admission();
        admission
            .answer(&request("/room", Some(&proof("room", "alice", 1, 2))))
            .unwrap();
        // Signs for the room with a key of its own: a valid proof, of another room's key.
        let stranger = proof("room", "mallory", 9, 8);
        assert_eq!(
            admission
                .answer(&request("/room", Some(&stranger)))
                .unwrap_err(),
            Refusal::RoomKey
        );
    }

    #[test]
    fn a_member_cannot_take_anothers_path() {
        let admission = admission();
        admission
            .answer(&request("/room", Some(&proof("room", "alice", 1, 2))))
            .unwrap();
        // Bob holds the room's key, and asks for alice's path with his own.
        let bob = proof("room", "alice", 1, 3);
        assert_eq!(
            admission.answer(&request("/room", Some(&bob))).unwrap_err(),
            Refusal::PathTaken
        );
        // At his own, he's let in, and can publish only there.
        let grant = admission
            .answer(&request("/room", Some(&proof("room", "bob", 1, 3))))
            .unwrap()
            .unwrap();
        assert_eq!(grant.publish, pattern("bob/**"));
    }

    #[test]
    fn a_session_comes_back_to_its_path() {
        let admission = admission();
        let proof = proof("room", "alice", 1, 2);
        for _ in 0..3 {
            admission.answer(&request("/room", Some(&proof))).unwrap();
        }
        let mut revalidate = request("/room", Some(&proof));
        revalidate.event = Event::Revalidate;
        admission.answer(&revalidate).unwrap().unwrap();
    }

    #[test]
    fn an_end_asks_for_nothing() {
        let mut end = request("/room", None);
        end.event = Event::End {
            reason: moq_auth::lease::Reason::Dropped,
            duration: Duration::ZERO,
            bytes: Default::default(),
        };
        assert_eq!(admission().answer(&end), Ok(None));
    }

    #[test]
    fn rooms_are_forgotten_and_then_held_by_whoever_comes_first() {
        let admission = admission();
        let start = Instant::now();
        let first = request("/room", Some(&proof("room", "alice", 1, 2)));
        admission.answer_at(&first, start).unwrap();
        let later = start + admission.policy.memory + Duration::from_secs(61);
        let new = request("/room", Some(&proof("room", "alice", 5, 6)));
        admission.answer_at(&new, later).unwrap();
    }

    #[test]
    fn the_server_has_limits() {
        let admission = Admission::new(Policy {
            rooms: 1,
            participants: 1,
            ..Policy::default()
        });
        admission
            .answer(&request("/a", Some(&proof("a", "alice", 1, 2))))
            .unwrap();
        assert_eq!(
            admission
                .answer(&request("/b", Some(&proof("b", "bob", 1, 2))))
                .unwrap_err(),
            Refusal::Full
        );
        assert_eq!(
            admission
                .answer(&request("/a", Some(&proof("a", "carol", 1, 3))))
                .unwrap_err(),
            Refusal::Full
        );
    }
}
