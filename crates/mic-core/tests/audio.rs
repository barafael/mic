//! Voices through `mic-testrelay`: each participant sends a tone of its own and hears every
//! other's, never its own, and still does after the relay restarts.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::{Client, Config, Connection, Invite, Media, Profile, Room, Trust};
use mic_media::audio::{
    Analyzer, AnalyzingSink, Mixer, Running, Sink, Source, ToneSource, capture,
};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

/// A client of `relay`; a connection gives up after 2 s, so a room's redials are quick to see.
fn client(relay: &TestRelay) -> Client {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    config.connect_timeout = Duration::from_secs(2);
    Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

/// A participant in the room, with a tone for a microphone if it has one, and an analyzer for a
/// speaker.
struct Participant {
    room: Room,
    heard: Analyzer,
    _running: Vec<Running>,
}

async fn join(relay: &TestRelay, invite: &Invite, name: &str, tone: Option<f64>) -> Participant {
    let mut running = Vec::new();
    let capture = tone.map(|tone| {
        let (writer, capture) = capture();
        running.push(Box::new(ToneSource::new(tone)).start(writer).unwrap());
        capture
    });
    let mixer = Mixer::new();
    let (sink, heard) = AnalyzingSink::new();
    running.push(Box::new(sink).start(mixer.clone()).unwrap());
    let media = Media {
        capture,
        mixer: Some(mixer),
    };
    let room = client(relay)
        .join_with(invite.clone(), Profile::new(name), media)
        .await
        .unwrap();
    Participant {
        room,
        heard,
        _running: running,
    }
}

/// Waits until `participant` hears `tone` at least at half its amplitude, and `own` hardly at
/// all.
async fn hears(participant: &Participant, tone: f64, own: f64) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let (theirs, mine) = (participant.heard.level(tone), participant.heard.level(own));
        if theirs > 0.25 && mine < 0.02 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{tone} Hz at {theirs}, its own {own} Hz at {mine}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn connection_is(room: &Room, connection: Connection) {
    let mut state = room.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.connection == connection),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn each_hears_the_other_and_not_itself_also_after_a_restart() {
    let mut relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&relay, &invite, "Alice", Some(440.0)).await;
    let bob = join(&relay, &invite, "Bob", Some(660.0)).await;
    hears(&bob, 440.0, 660.0).await;
    hears(&alice, 660.0, 440.0).await;

    relay.stop().await.unwrap();
    connection_is(&alice.room, Connection::Reconnecting).await;
    // Longer than a connection's redial window, so the rooms dial anew; and longer than the
    // analyzers' window, so what they hear after is what came after.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(bob.heard.level(440.0) < 0.02, "{}", bob.heard.level(440.0));
    relay.restart().await.unwrap();
    connection_is(&alice.room, Connection::Connected).await;
    connection_is(&bob.room, Connection::Connected).await;
    hears(&bob, 440.0, 660.0).await;
    hears(&alice, 660.0, 440.0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_without_a_microphone_only_listens() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&relay, &invite, "Alice", Some(440.0)).await;
    let carol = join(&relay, &invite, "Carol", None).await;
    hears(&carol, 440.0, 660.0).await;
    // Carol has no voice for Alice to hear.
    assert!(alice.heard.rms() < 0.01, "{}", alice.heard.rms());
}
