//! Who is speaking, and who the active speaker is (§1.6, §5.7): worked out by each client from
//! the voices it decodes, since the relay can't hear them under E2EE. A voice is speaking while
//! its packets come loud, and for a moment after, so the gaps between words don't count as
//! silence. The active speaker is the one speaking, kept while they go on, and kept after they
//! stop until someone else speaks: a room's tiles don't jump at every pause.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::roster::ParticipantKey;

/// How loud a packet must be to count as speech: an RMS of -40 dBFS.
pub const LOUD: f32 = 0.01;
/// How long a voice counts as speaking after its last loud packet.
pub const HOLD: Duration = Duration::from_millis(500);
/// How often the room looks at who speaks.
pub const LOOK: Duration = Duration::from_millis(100);

/// When each voice was last loud, as the voices' players tell it. Cheap to clone; every clone is
/// the same.
#[derive(Clone, Default)]
pub(crate) struct Voices {
    loud: Arc<Mutex<HashMap<ParticipantKey, Instant>>>,
}

impl Voices {
    /// `whose` voice played `samples` just now.
    pub fn heard(&self, whose: ParticipantKey, samples: &[f32]) {
        if rms(samples) >= LOUD {
            self.loud
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(whose, Instant::now());
        }
    }

    /// Those speaking at `now`, among `present`, in `present`'s order.
    pub fn speaking(&self, present: &[ParticipantKey], now: Instant) -> Vec<ParticipantKey> {
        let loud = self.loud.lock().unwrap_or_else(PoisonError::into_inner);
        present
            .iter()
            .filter(|key| {
                loud.get(key)
                    .is_some_and(|last| now.saturating_duration_since(*last) < HOLD)
            })
            .copied()
            .collect()
    }

    /// When `whose` voice was last loud, if ever.
    fn last_loud(&self, whose: &ParticipantKey) -> Option<Instant> {
        self.loud
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(whose)
            .copied()
    }

    /// The active speaker at `now`, among `present`, given the last one: see [`choose`].
    pub fn active(
        &self,
        last: Option<ParticipantKey>,
        present: &[ParticipantKey],
        now: Instant,
    ) -> Option<ParticipantKey> {
        let speaking = self.speaking(present, now);
        choose(last, present, &speaking, |key| self.last_loud(key))
    }
}

/// The active speaker: `last`, while they speak or nobody else does, if they're still `present`;
/// else whoever of `speaking` was loud most recently.
fn choose(
    last: Option<ParticipantKey>,
    present: &[ParticipantKey],
    speaking: &[ParticipantKey],
    last_loud: impl Fn(&ParticipantKey) -> Option<Instant>,
) -> Option<ParticipantKey> {
    let last = last.filter(|key| present.contains(key));
    if let Some(key) = last
        && (speaking.contains(&key) || speaking.is_empty())
    {
        return Some(key);
    }
    speaking
        .iter()
        .max_by_key(|key| last_loud(key))
        .copied()
        .or(last)
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> ParticipantKey {
        ParticipantKey::from_bytes([byte; 32])
    }

    #[test]
    fn a_loud_voice_speaks_and_stops_after_the_hold() {
        let voices = Voices::default();
        let (alice, bob) = (key(1), key(2));
        voices.heard(alice, &[0.5; 960]);
        voices.heard(bob, &[0.001; 960]);
        let now = Instant::now();
        assert_eq!(voices.speaking(&[alice, bob], now), [alice]);
        assert!(voices.speaking(&[alice, bob], now + HOLD).is_empty());
        // One who left isn't speaking, whatever they said last.
        assert!(voices.speaking(&[bob], now).is_empty());
    }

    #[test]
    fn the_speaker_stays_while_speaking_and_after_until_another_speaks() {
        let (alice, bob, carol) = (key(1), key(2), key(3));
        let present = [alice, bob, carol];
        let start = Instant::now();
        let loud = |key: &ParticipantKey| {
            Some(start + Duration::from_millis(u64::from(key.as_bytes()[0])))
        };
        // Both speak: the one already speaking stays.
        assert_eq!(
            choose(Some(alice), &present, &[alice, bob], loud),
            Some(alice)
        );
        // She stops, he goes on: he takes over.
        assert_eq!(choose(Some(alice), &present, &[bob], loud), Some(bob));
        // Nobody speaks: the last stays.
        assert_eq!(choose(Some(bob), &present, &[], loud), Some(bob));
        // No speaker yet: the most recently loud of those speaking.
        assert_eq!(choose(None, &present, &[alice, carol], loud), Some(carol));
        assert_eq!(choose(None, &present, &[], loud), None);
        // The last left: someone else, or nobody.
        assert_eq!(choose(Some(alice), &[bob], &[], loud), None);
    }
}
