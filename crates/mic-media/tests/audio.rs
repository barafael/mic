//! Sources to sinks in real time, as a call wires them: tones captured a frame at a time on the
//! async side, written into the mixer's streams, and heard by the analyzing sink.

use std::time::Duration;

use mic_media::audio::{AnalyzingSink, Mixer, Sink, Source, ToneSource, capture};

/// Plays each tone's capture into a stream of its own for `duration`; returns what was heard.
async fn play(tones: &[f64], duration: Duration) -> mic_media::audio::Analyzer {
    let mixer = Mixer::new();
    let (sink, analyzer) = AnalyzingSink::new();
    let _playing = Box::new(sink).start(mixer.clone()).unwrap();
    let mut forwarding = Vec::new();
    let mut sources = Vec::new();
    for &frequency in tones {
        let (writer, mut capture) = capture();
        sources.push(Box::new(ToneSource::new(frequency)).start(writer).unwrap());
        let mut playout = mixer.stream();
        forwarding.push(tokio::spawn(async move {
            while let Some(frame) = capture.frame().await {
                playout.write(&frame);
            }
        }));
    }
    tokio::time::sleep(duration).await;
    drop(sources);
    for forwarding in forwarding {
        forwarding.await.unwrap();
    }
    analyzer
}

#[tokio::test(flavor = "multi_thread")]
async fn each_tone_is_heard_and_nothing_else() {
    let heard = play(&[440.0, 660.0], Duration::from_millis(1500)).await;
    for frequency in [440.0, 660.0] {
        // A tone of amplitude 0.5, give or take the jitter buffer's start and the end.
        let level = heard.level(frequency);
        assert!(level > 0.35, "{frequency} Hz at {level}");
    }
    let level = heard.level(550.0);
    assert!(level < 0.05, "550 Hz at {level}");
    assert!(
        heard.played() >= 48_000,
        "{} samples played",
        heard.played()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_tone_is_silence() {
    let heard = play(&[], Duration::from_millis(300)).await;
    assert_eq!(heard.rms(), 0.0);
}
