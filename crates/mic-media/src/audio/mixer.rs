//! The playback side: a [`Playout`] per stream the async side writes decoded audio into, and the
//! [`Mixer`] a sink's thread sums them through.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use ringbuf::traits::{Consumer as _, Observer as _, Producer as _, Split as _};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::{FRAME, SAMPLE_RATE};

/// How much a stream's ring holds: half a second.
const CAPACITY: usize = SAMPLE_RATE as usize / 2;
/// How much a stream holds before it plays, and again after it ran dry: two frames, a small
/// jitter buffer, so a stream that arrives in bursts plays smoothly instead of in pieces.
const PREBUFFER: usize = 2 * FRAME;

/// What a sink plays: every stream, summed. Cheap to clone; every clone is the same.
#[derive(Clone, Default)]
pub struct Mixer {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    streams: Mutex<Vec<Stream>>,
    next_id: AtomicU64,
    /// Fills that found the lock taken (a stream coming or going), and so played silence.
    contended: AtomicU64,
}

struct Stream {
    id: u64,
    consumer: HeapCons<f32>,
    /// Whether it's playing, or still filling its jitter buffer.
    playing: bool,
}

impl Mixer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A stream to write decoded audio into; it's mixed in until the [`Playout`] is dropped.
    pub fn stream(&self) -> Playout {
        let (producer, consumer) = HeapRb::<f32>::new(CAPACITY).split();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Stream {
                id,
                consumer,
                playing: false,
            });
        Playout {
            mixer: self.clone(),
            id,
            producer,
            dropped: 0,
        }
    }

    /// Fills `out` with every stream, summed, for a sink's thread: it never waits for the async
    /// side, and never allocates or frees. A stream short of samples gives what it has, and is
    /// silent from there until its jitter buffer fills again.
    pub fn fill(&self, out: &mut [f32]) {
        out.fill(0.0);
        let Ok(mut streams) = self.inner.streams.try_lock() else {
            self.inner.contended.fetch_add(1, Ordering::Relaxed);
            return;
        };
        for stream in streams.iter_mut() {
            if !stream.playing {
                if stream.consumer.occupied_len() < PREBUFFER {
                    continue;
                }
                stream.playing = true;
            }
            let available = stream.consumer.occupied_len();
            // `out` first: the zip stops on it without taking a sample more.
            for (sample, from) in out.iter_mut().zip(stream.consumer.pop_iter()) {
                *sample += from;
            }
            if available < out.len() {
                stream.playing = false;
            }
        }
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    /// How many streams it mixes.
    pub fn streams(&self) -> usize {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// How many fills played silence because a stream was coming or going.
    pub fn contended(&self) -> u64 {
        self.inner.contended.load(Ordering::Relaxed)
    }
}

/// A stream into the mixer, for the async side: it writes decoded audio, which the sink plays in
/// order. Dropping it takes the stream out.
pub struct Playout {
    mixer: Mixer,
    id: u64,
    producer: HeapProd<f32>,
    dropped: u64,
}

impl Playout {
    /// Writes `samples` to be played after what's there; what doesn't fit, because the sink is
    /// half a second behind, is dropped and counted.
    pub fn write(&mut self, samples: &[f32]) {
        let written = self.producer.push_slice(samples);
        self.dropped += (samples.len() - written) as u64;
    }

    /// How many samples didn't fit.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// How many samples wait to be played.
    pub fn queued(&self) -> usize {
        self.producer.occupied_len()
    }
}

impl Drop for Playout {
    /// Takes the stream out, here on the async side, so the sink's thread never frees it.
    fn drop(&mut self) {
        self.mixer
            .inner
            .streams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|stream| stream.id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_are_summed_once_their_jitter_buffers_fill() {
        let mixer = Mixer::new();
        let mut one = mixer.stream();
        let mut two = mixer.stream();
        one.write(&vec![0.25; PREBUFFER]);
        two.write(&vec![0.5; PREBUFFER - 1]);
        let mut out = vec![1.0; FRAME];
        mixer.fill(&mut out);
        assert!(
            out.iter().all(|&sample| sample == 0.25),
            "two isn't playing yet"
        );
        two.write(&[0.5]);
        mixer.fill(&mut out);
        assert!(out.iter().all(|&sample| sample == 0.75));
    }

    #[test]
    fn a_stream_that_runs_dry_waits_to_fill_again() {
        let mixer = Mixer::new();
        let mut one = mixer.stream();
        one.write(&vec![0.5; PREBUFFER + 10]);
        let mut out = vec![0.0; PREBUFFER];
        mixer.fill(&mut out);
        let mut out = vec![0.0; FRAME];
        mixer.fill(&mut out);
        assert_eq!(out.iter().filter(|&&sample| sample == 0.5).count(), 10);
        assert!(out[10..].iter().all(|&sample| sample == 0.0));
        one.write(&vec![0.5; FRAME]);
        mixer.fill(&mut out);
        assert!(
            out.iter().all(|&sample| sample == 0.0),
            "below the buffer again"
        );
    }

    #[test]
    fn the_sum_is_clamped_and_a_dropped_stream_is_gone() {
        let mixer = Mixer::new();
        let mut loud = [mixer.stream(), mixer.stream()];
        for stream in &mut loud {
            stream.write(&vec![0.8; PREBUFFER]);
        }
        let mut out = vec![0.0; FRAME];
        mixer.fill(&mut out);
        assert!(out.iter().all(|&sample| sample == 1.0));
        drop(loud);
        assert_eq!(mixer.streams(), 0);
    }
}
