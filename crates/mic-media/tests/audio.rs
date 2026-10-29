//! Sources to sinks in real time, as a call wires them: tones captured a frame at a time on the
//! async side, written into the mixer's streams, and heard by the analyzing sink.

use std::time::Duration;

use mic_media::audio::{AnalyzingSink, Mixer, Sink, Source, ToneSource, capture};

/// Plays each tone's capture into a stream of its own for `duration`, and has `heard` look at
/// what was heard while they still play: once they stop, the sink plays on, and silence fills
/// its window (on a slow machine, CI's macOS runner, half of it).
async fn play(tones: &[f64], duration: Duration, heard: impl FnOnce(&mic_media::audio::Analyzer)) {
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
    heard(&analyzer);
    drop(sources);
    for forwarding in forwarding {
        forwarding.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_tone_is_heard_and_nothing_else() {
    play(&[440.0, 660.0], Duration::from_millis(1500), |heard| {
        // Each tone heard, well over what isn't played. Not its whole amplitude, 0.5: on a
        // shared machine (CI's macOS runner) the synthetic source and sink, each a thread of its
        // own, don't keep real time, and the gaps between them lower the level (0.18 there).
        let between = heard.level(550.0);
        assert!(between < 0.05, "550 Hz at {between}");
        for frequency in [440.0, 660.0] {
            let level = heard.level(frequency);
            assert!(
                level > 0.15 && level > between * 5.0,
                "{frequency} Hz at {level}, 550 Hz at {between}"
            );
        }
        assert!(
            heard.played() >= 48_000,
            "{} samples played",
            heard.played()
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_tone_is_silence() {
    play(&[], Duration::from_millis(300), |heard| {
        assert_eq!(heard.rms(), 0.0);
    })
    .await;
}
