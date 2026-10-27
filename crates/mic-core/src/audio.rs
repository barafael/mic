//! A participant's voice: Opus on a track of its broadcast, `audio`, encrypted and signed like
//! every other (D17, D18), from a [`Capture`]; and every other participant's, decoded into a
//! [`Mixer`] stream of its own.
//!
//! One format in v1: mono, 48 kHz, 20 ms packets (mic-media's), so a subscriber builds its
//! decoder from the same settings a publisher encodes with, and needs no codec description from
//! the catalog. Codecs are negotiated once there are two (PLAN.md, commit 31).
//!
//! **Groups of a second.** moq-audio puts every packet in a group of its own, which keeps one lost
//! packet from holding up the next; but every group is an entry in the checkpoints (`crate::sig`),
//! 50 a second, each listed for 3 s. A group of a second's packets lists one or two. The price:
//! a packet lost on the way holds up the rest of its group until it's sent again, which the
//! stream's jitter buffer and its cap on what it queues ride out. The `network` phase (M8) will
//! say what that costs under loss.
//!
//! **What a stream may queue.** A subscriber that comes in the middle of a group gets the group
//! from its start, a second at once. Played, it would put that listener a second behind for
//! good; so a stream holding more than [`QUEUE`] drops what comes, which plays the start of the
//! backlog and then the live edge. The same cap keeps a sender's clock that runs a little fast
//! from building up delay.

use mic_media::audio::noise::{NoiseSuppression, Suppressor};
use mic_media::audio::{Capture, FRAME, Mixer, SAMPLE_RATE};
use moq_audio::Layout;
use moq_audio::decode::Decoder;
use moq_audio::encode::{Encoder, Settings};

use crate::sig::{SignedTrack, VerifiedTrack};
use crate::tracks::TrackError;

/// The voice's track's semantic name.
pub const AUDIO: &str = "audio";
/// Packets a group: a second's.
const GROUP: u64 = 50;
/// A packet's duration on the media timeline.
const PACKET_MICROS: u64 = 20_000;
/// The most a stream queues before it drops what comes: 120 ms, six packets.
const QUEUE: usize = 6 * FRAME;

/// What the voice is encoded with, and so decoded with.
fn settings() -> Settings {
    Settings::new(SAMPLE_RATE, Layout::Mono)
}

fn codec(error: moq_audio::Error) -> TrackError {
    TrackError::Codec(error.to_string())
}

/// What a subscriber decodes a publisher's voice with: the same settings, so the same rendition.
fn decoder() -> Result<Decoder, TrackError> {
    let catalog = Encoder::new(&settings()).map_err(codec)?.catalog();
    Decoder::new(&catalog, &moq_audio::decode::Config::new()).map_err(codec)
}

/// Encodes what `capture` gives onto `track`, a group a second, until the capture ends or the
/// track fails, its noise taken out by the suppressor `noise` names. While `muted` says so, what
/// it gives is read and sent nowhere: the far end hears nothing.
pub(crate) async fn publish(
    mut capture: Capture,
    mut track: SignedTrack,
    muted: tokio::sync::watch::Receiver<bool>,
    mut noise: tokio::sync::watch::Receiver<NoiseSuppression>,
) {
    let mut suppressor = Suppressor::new(*noise.borrow_and_update());
    let mut encoder = match Encoder::new(&settings()) {
        Ok(encoder) => encoder,
        Err(error) => {
            tracing::warn!(%error, "no Opus encoder: the voice isn't sent");
            return;
        }
    };
    debug_assert_eq!(encoder.frame_size(), FRAME);
    let mut group = None;
    let mut packets: u64 = 0;
    while let Some(mut frame) = capture.frame().await {
        if noise.has_changed().unwrap_or(false) {
            suppressor = Suppressor::new(*noise.borrow_and_update());
        }
        if *muted.borrow() {
            continue;
        }
        suppressor.process(&mut frame);
        let packet = match encoder.encode(&frame) {
            Ok(packet) => packet,
            Err(error) => {
                tracing::warn!(%error, "encoding the voice");
                continue;
            }
        };
        if packets.is_multiple_of(GROUP)
            && let Some(full) = group.take()
            && let Err(error) = crate::sig::SignedGroup::finish(full)
        {
            tracing::debug!(%error, "finishing a voice group");
        }
        if group.is_none() {
            match track.append_group() {
                Ok(new) => group = Some(new),
                Err(error) => {
                    tracing::debug!(%error, "the voice track ended");
                    return;
                }
            }
        }
        // The media timeline, from the packets sent: max age is measured on it.
        let timestamp = match moq_net::Timestamp::from_micros(PACKET_MICROS * packets) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                tracing::warn!(%error, "the voice's timeline ran out");
                return;
            }
        };
        if let Some(open) = group.as_mut()
            && let Err(error) = open.write_frame(timestamp, &packet.payload)
        {
            tracing::debug!(%error, "the voice track ended");
            return;
        }
        packets += 1;
    }
    if let Some(open) = group
        && let Err(error) = open.finish()
    {
        tracing::debug!(%error, "finishing the last voice group");
    }
}

/// Decodes `track`, someone's voice, into a stream of `mixer`'s, until the track ends; the
/// stream goes with it.
pub(crate) async fn play(mut track: VerifiedTrack, mixer: Mixer) {
    let mut decoder = match decoder() {
        Ok(decoder) => decoder,
        Err(error) => {
            tracing::warn!(%error, "no Opus decoder: a voice isn't played");
            return;
        }
    };
    let mut stream = mixer.stream();
    loop {
        let mut group = match track.recv_group().await {
            Ok(Some(group)) => group,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error, "a voice track failed");
                return;
            }
        };
        loop {
            let frame = match group.read_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    tracing::debug!(%error, "a voice group failed");
                    break;
                }
            };
            // Decoded all the same, so the decoder's state follows the stream.
            match decoder.decode(&frame.plaintext) {
                Ok(decoded) if stream.queued() <= QUEUE => stream.write(&decoded.samples),
                Ok(_) => {}
                Err(error) => tracing::debug!(%error, "a voice packet didn't decode"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_decodes_with_the_subscribers_own_decoder() {
        let mut encoder = Encoder::new(&settings()).unwrap();
        let mut decoder = decoder().unwrap();
        let tone: Vec<f32> = (0..FRAME)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.5)
            .collect();
        let mut decoded = Vec::new();
        for _ in 0..10 {
            let packet = encoder.encode(&tone).unwrap();
            decoded.extend(decoder.decode(&packet.payload).unwrap().samples);
        }
        // Less only the decoder's start-up delay, which it trims.
        assert!(decoded.len() >= 9 * FRAME, "{}", decoded.len());
        let loudest = decoded.iter().fold(0.0_f32, |max, s| max.max(s.abs()));
        assert!(loudest > 0.3, "{loudest}");
    }
}
