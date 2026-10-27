//! The capture ring: a source's thread writes, the async side reads a frame at a time.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ringbuf::traits::{Consumer as _, Observer as _, Producer as _, Split as _};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::FRAME;

/// How much a ring holds: half a second. A reader further behind than that loses the oldest of
/// it; a live call has no use for older audio.
const CAPACITY: usize = super::SAMPLE_RATE as usize / 2;
/// How often the reader looks for a whole frame: a quarter frame.
const LOOK: Duration = Duration::from_millis(5);

/// A ring from a source to the async side: its writing end, and its reading end.
pub fn capture() -> (Writer, Capture) {
    let (producer, consumer) = HeapRb::<f32>::new(CAPACITY).split();
    let dropped = Arc::new(AtomicU64::new(0));
    (
        Writer {
            producer,
            dropped: dropped.clone(),
        },
        Capture {
            consumer,
            dropped,
            look: None,
        },
    )
}

/// A ring's writing end, for a source's thread: it never blocks or allocates.
pub struct Writer {
    producer: HeapProd<f32>,
    dropped: Arc<AtomicU64>,
}

impl Writer {
    /// Writes `samples`; what doesn't fit, because the reader is behind, is dropped and counted.
    pub fn write(&mut self, samples: &[f32]) {
        let written = self.producer.push_slice(samples);
        if written < samples.len() {
            self.dropped
                .fetch_add((samples.len() - written) as u64, Ordering::Relaxed);
        }
    }
}

/// A ring's reading end, for the async side.
pub struct Capture {
    consumer: HeapCons<f32>,
    dropped: Arc<AtomicU64>,
    look: Option<tokio::time::Interval>,
}

impl Capture {
    /// The next frame, once a whole one is there; none once the source is gone and what it
    /// wrote is read. Cancel-safe: samples are taken only once a whole frame is there.
    pub async fn frame(&mut self) -> Option<Vec<f32>> {
        loop {
            if let Some(frame) = self.try_frame() {
                return Some(frame);
            }
            if self.consumer.write_is_held() {
                self.look
                    .get_or_insert_with(|| {
                        let mut look = tokio::time::interval(LOOK);
                        look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        look
                    })
                    .tick()
                    .await;
            } else {
                // The source is gone, perhaps after its last frame since the look above.
                return self.try_frame();
            }
        }
    }

    /// The next frame, if a whole one is there now.
    pub fn try_frame(&mut self) -> Option<Vec<f32>> {
        if self.consumer.occupied_len() < FRAME {
            return None;
        }
        let mut frame = vec![0.0; FRAME];
        let read = self.consumer.pop_slice(&mut frame);
        debug_assert_eq!(read, FRAME);
        Some(frame)
    }

    /// How many samples were dropped because this end was behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_come_whole_and_in_order() {
        let (mut writer, mut capture) = capture();
        let samples: Vec<f32> = (0..FRAME * 2 + 10).map(|i| i as f32).collect();
        // Written in pieces that don't line up with frames, as a device's callbacks come.
        for piece in samples.chunks(333) {
            writer.write(piece);
        }
        assert_eq!(capture.frame().await.unwrap(), samples[..FRAME]);
        assert_eq!(capture.frame().await.unwrap(), samples[FRAME..FRAME * 2]);
        assert!(capture.try_frame().is_none(), "10 samples aren't a frame");
        drop(writer);
        assert!(capture.frame().await.is_none(), "the source is gone");
        assert_eq!(capture.dropped(), 0);
    }

    #[test]
    fn a_reader_far_behind_loses_samples_and_counts_them() {
        let (mut writer, capture) = capture();
        writer.write(&vec![0.5; CAPACITY + 100]);
        assert_eq!(capture.dropped(), 100);
    }
}
