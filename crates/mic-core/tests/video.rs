//! Cameras through `mic-testrelay`: each participant decodes every other's, the pattern names the
//! right sender, the frame rate holds, and it does again after the relay restarts; one that
//! decodes nothing decodes nothing, and leaves the others their video.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::video::CAMERA_SETTINGS;
use mic_core::{Client, Config, Connection, Invite, Media, ParticipantKey, Profile, Room, Trust};
use mic_media::audio::Running;
use mic_media::video::{PatternCamera, read_pattern};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(15);

fn config(relay: &TestRelay) -> Config {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    config.connect_timeout = Duration::from_secs(2);
    config
}

struct Participant {
    room: Room,
    _camera: Option<Running>,
}

async fn join(invite: &Invite, name: &str, camera: Option<u8>, config: Config) -> Participant {
    let (camera, running) = match camera {
        Some(sender) => {
            let (pictures, shown) = tokio::sync::watch::channel(None);
            let running = PatternCamera {
                sender,
                width: CAMERA_SETTINGS.width,
                height: CAMERA_SETTINGS.height,
                fps: CAMERA_SETTINGS.fps,
            }
            .start(pictures)
            .unwrap();
            (Some(shown), Some(running))
        }
        None => (None, None),
    };
    let client = Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    let room = client
        .join_with(
            invite.clone(),
            Profile::new(name),
            Media {
                camera,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    Participant {
        room,
        _camera: running,
    }
}

/// Waits until `viewer` decodes `whose` camera, showing `sender`'s pattern; returns how many
/// pictures it had decoded by then.
async fn sees(viewer: &Participant, whose: ParticipantKey, sender: u8) -> u64 {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let read = viewer
            .room
            .video(whose)
            .borrow()
            .as_deref()
            .and_then(read_pattern);
        if read.is_some_and(|pattern| pattern.sender == sender) {
            return viewer.room.video_count(whose);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no picture of sender {sender}: {read:?}, {} decoded",
            viewer.room.video_count(whose)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// How many of `whose` pictures `viewer` decodes a second, over two seconds.
async fn rate(viewer: &Participant, whose: ParticipantKey) -> f64 {
    let before = viewer.room.video_count(whose);
    tokio::time::sleep(Duration::from_secs(2)).await;
    (viewer.room.video_count(whose) - before) as f64 / 2.0
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
async fn each_decodes_the_others_camera_also_after_a_restart() {
    let mut relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&invite, "Alice", Some(1), config(&relay)).await;
    let bob = join(&invite, "Bob", Some(2), config(&relay)).await;
    let (alices, bobs) = (alice.room.participant(), bob.room.participant());
    sees(&bob, alices, 1).await;
    sees(&alice, bobs, 2).await;
    let fps = f64::from(CAMERA_SETTINGS.fps);
    let rate = rate(&bob, alices).await;
    assert!(rate > fps * 0.7, "{rate} pictures a second, of {fps}");

    relay.stop().await.unwrap();
    connection_is(&alice.room, Connection::Reconnecting).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    relay.restart().await.unwrap();
    connection_is(&alice.room, Connection::Connected).await;
    connection_is(&bob.room, Connection::Connected).await;
    let before = bob.room.video_count(alices);
    sees(&bob, alices, 1).await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while bob.room.video_count(alices) < before + 15 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no pictures since the restart"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_who_decodes_nothing_decodes_nothing_and_the_others_still_see() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let mut blind = config(&relay);
    blind.decoders = Vec::new();
    let dave = join(&invite, "Dave", None, blind).await;
    let alice = join(&invite, "Alice", Some(1), config(&relay)).await;
    let bob = join(&invite, "Bob", Some(2), config(&relay)).await;
    sees(&bob, alice.room.participant(), 1).await;
    sees(&alice, bob.room.participant(), 2).await;
    // Long enough to have decoded some, had Dave decoded anything.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(dave.room.video_count(alice.room.participant()), 0);
    // He knows what he can't decode: Alice's camera, in VP9.
    let state = dave.room.state().borrow().clone();
    let theirs = state
        .participants
        .iter()
        .find(|them| them.key == alice.room.participant())
        .unwrap();
    assert_eq!(theirs.renditions[0].codec, "vp9");
    assert!(state.me.decoders.is_empty());
}
