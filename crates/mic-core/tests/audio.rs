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
        ..Default::default()
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

/// Waits until `participant` hears `tone` hardly at all.
async fn silent(participant: &Participant, tone: f64) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while participant.heard.level(tone) >= 0.02 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{tone} Hz still at {}",
            participant.heard.level(tone)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn muted_is_silence_on_the_far_end_and_said_in_the_profile() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&relay, &invite, "Alice", Some(440.0)).await;
    let bob = join(&relay, &invite, "Bob", Some(660.0)).await;
    hears(&bob, 440.0, 660.0).await;

    alice
        .room
        .command(mic_core::Command::SetMuted { muted: true })
        .await
        .unwrap();
    assert!(alice.room.state().borrow().me.profile.muted);
    let mut state = bob.room.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.iter().any(|them| them.profile.muted)),
    )
    .await
    .unwrap()
    .unwrap();
    silent(&bob, 440.0).await;
    // Alice still hears Bob: muting is hers alone.
    hears(&alice, 660.0, 440.0).await;

    alice
        .room
        .command(mic_core::Command::SetMuted { muted: false })
        .await
        .unwrap();
    hears(&bob, 440.0, 660.0).await;
    assert!(!bob.room.state().borrow().participants[0].profile.muted);
}

/// Waits until `listener`'s room says `holds` of its active speaker and who's speaking.
async fn speaker_is(
    listener: &Participant,
    what: &str,
    holds: impl Fn(Option<mic_core::ParticipantKey>, &[mic_core::ParticipantKey]) -> bool,
) {
    let mut state = listener.room.state();
    let waited = tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| holds(state.speaker, &state.speaking)),
    )
    .await
    .is_ok();
    assert!(waited, "{what}: {:?}", *listener.room.state().borrow());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_active_speaker_is_who_speaks_and_stays_through_silence() {
    use mic_core::Command;
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&relay, &invite, "Alice", Some(440.0)).await;
    let bob = join(&relay, &invite, "Bob", Some(660.0)).await;
    let carol = join(&relay, &invite, "Carol", None).await;
    let (alices, bobs) = (alice.room.participant(), bob.room.participant());
    bob.room
        .command(Command::SetMuted { muted: true })
        .await
        .unwrap();
    speaker_is(&carol, "alice speaks", |speaker, speaking| {
        speaker == Some(alices) && speaking == [alices]
    })
    .await;
    // She stops, he starts: he takes over.
    alice
        .room
        .command(Command::SetMuted { muted: true })
        .await
        .unwrap();
    bob.room
        .command(Command::SetMuted { muted: false })
        .await
        .unwrap();
    speaker_is(&carol, "bob speaks", |speaker, speaking| {
        speaker == Some(bobs) && speaking == [bobs]
    })
    .await;
    // Nobody speaks: he stays the speaker.
    bob.room
        .command(Command::SetMuted { muted: true })
        .await
        .unwrap();
    speaker_is(&carol, "nobody speaks", |speaker, speaking| {
        speaker == Some(bobs) && speaking.is_empty()
    })
    .await;
}
