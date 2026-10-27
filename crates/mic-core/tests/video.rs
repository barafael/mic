//! Cameras through `mic-testrelay`: each participant decodes every other's, the pattern names the
//! right sender, the frame rate holds, and it does again after the relay restarts; one that
//! decodes nothing decodes nothing, and leaves the others their video.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::video::CAMERA_SETTINGS;
use mic_core::{Client, Config, Connection, Invite, Media, ParticipantKey, Profile, Room, Trust};
use mic_media::audio::Running;
use mic_media::video::{Pattern, PatternCamera, read_pattern};
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
    join_sending(invite, name, camera, config, false).await
}

/// As [`join`], the camera recorded once and sent in a loop if `recorded`.
async fn join_sending(
    invite: &Invite,
    name: &str,
    camera: Option<u8>,
    config: Config,
    recorded: bool,
) -> Participant {
    let (camera, running) = match camera {
        Some(sender) if recorded => {
            let pictures = (0..CAMERA_SETTINGS.fps * 2).map(|frame| {
                Pattern {
                    sender,
                    frame: frame as u16,
                }
                .draw(CAMERA_SETTINGS.width, CAMERA_SETTINGS.height)
            });
            let recorded = mic_core::video::Recorded::encode(pictures).unwrap();
            (Some(mic_core::video::Camera::Recorded(recorded)), None)
        }
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
            (Some(mic_core::video::Camera::Live(shown)), Some(running))
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
async fn a_recorded_camera_is_seen_at_its_rate() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let crowd = join_sending(&invite, "Crowd", Some(7), config(&relay), true).await;
    let bob = join(&invite, "Bob", None, config(&relay)).await;
    let whose = crowd.room.participant();
    sees(&bob, whose, 7).await;
    let fps = f64::from(CAMERA_SETTINGS.fps);
    let rate = rate(&bob, whose).await;
    assert!(rate > fps * 0.7, "{rate} pictures a second, of {fps}");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_cameras_watched_are_decoded() {
    use mic_core::Command;
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&invite, "Alice", Some(1), config(&relay)).await;
    let bob = join(&invite, "Bob", Some(2), config(&relay)).await;
    let carol = join(&invite, "Carol", None, config(&relay)).await;
    let (alices, bobs) = (alice.room.participant(), bob.room.participant());
    sees(&carol, alices, 1).await;
    sees(&carol, bobs, 2).await;
    carol
        .room
        .command(Command::Watch {
            participants: Some(vec![alices]),
        })
        .await
        .unwrap();
    // What was on its way when the camera was dropped.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let fps = f64::from(CAMERA_SETTINGS.fps);
    let (of_alice, of_bob) = (rate(&carol, alices).await, rate(&carol, bobs).await);
    assert!(of_alice > fps * 0.7, "alice's at {of_alice} a second");
    assert_eq!(of_bob, 0.0, "bob's, unwatched");
    carol
        .room
        .command(Command::Watch { participants: None })
        .await
        .unwrap();
    let before = carol.room.video_count(bobs);
    sees(&carol, bobs, 2).await;
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while carol.room.video_count(bobs) < before + 15 {
        assert!(
            carol.room.state().borrow().rejected.is_empty(),
            "bob's broadcast rejected: {:?}",
            carol.room.state().borrow().rejected
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "bob's camera not back: {:?}",
            *carol.room.state().borrow()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_screen_is_seen_until_it_stops() {
    use mic_core::Command;
    use mic_core::video::{SCREEN_SETTINGS, ScreenSource};
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let screen: ScreenSource = std::sync::Arc::new(|pictures| {
        PatternCamera {
            sender: 77,
            width: SCREEN_SETTINGS.width,
            height: SCREEN_SETTINGS.height,
            fps: SCREEN_SETTINGS.fps,
        }
        .start(pictures)
        .map_err(|error| error.to_string())
    });
    let alice = Client::new(
        config(&relay),
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
    .join_with(
        invite.clone(),
        Profile::new("Alice"),
        Media {
            screen: Some(screen),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let bob = join(&invite, "Bob", None, config(&relay)).await;
    let whose = alice.participant();
    alice
        .command(Command::ShareScreen { on: true })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let read = bob
            .room
            .screen(whose)
            .borrow()
            .as_deref()
            .and_then(read_pattern);
        if read.is_some_and(|pattern| pattern.sender == 77) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no screen: {read:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut state = bob.room.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.iter().any(|them| them.profile.sharing)),
    )
    .await
    .unwrap()
    .unwrap();
    alice
        .command(Command::ShareScreen { on: false })
        .await
        .unwrap();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.iter().all(|them| !them.profile.sharing)),
    )
    .await
    .unwrap()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = bob.room.screen_count(whose);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        bob.room.screen_count(whose),
        before,
        "pictures after it stopped"
    );
    // And again.
    alice
        .command(Command::ShareScreen { on: true })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while bob.room.screen_count(whose) < before + 5 {
        assert!(tokio::time::Instant::now() < deadline, "no screen again");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // One with nothing to share says so.
    assert!(
        bob.room
            .command(Command::ShareScreen { on: true })
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_share_whose_capture_ends_stops_for_everyone() {
    use mic_core::Command;
    use mic_core::video::{SCREEN_SETTINGS, ScreenSource};
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    // A capture that sends a few pictures and ends, as a screen that goes away does.
    let screen: ScreenSource = std::sync::Arc::new(|pictures| {
        std::thread::spawn(move || {
            for frame in 0..10 {
                let picture = Pattern { sender: 78, frame }
                    .draw(SCREEN_SETTINGS.width, SCREEN_SETTINGS.height);
                pictures.send_replace(Some(std::sync::Arc::new(picture)));
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Ok(Running::new(|| {}))
    });
    let alice = Client::new(
        config(&relay),
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
    .join_with(
        invite.clone(),
        Profile::new("Alice"),
        Media {
            screen: Some(screen),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let bob = join(&invite, "Bob", None, config(&relay)).await;
    alice
        .command(Command::ShareScreen { on: true })
        .await
        .unwrap();
    assert!(alice.state().borrow().me.profile.sharing);
    // It stops by itself, for Alice and for Bob.
    let mut mine = alice.state();
    tokio::time::timeout(PATIENCE, mine.wait_for(|state| !state.me.profile.sharing))
        .await
        .unwrap()
        .unwrap();
    let mut theirs = bob.room.state();
    tokio::time::timeout(
        PATIENCE,
        theirs.wait_for(|state| {
            !state.participants.is_empty()
                && state.participants.iter().all(|them| !them.profile.sharing)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    // And starts again when asked.
    alice
        .command(Command::ShareScreen { on: true })
        .await
        .unwrap();
    assert!(alice.state().borrow().me.profile.sharing);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_share_waiting_on_the_desktop_leaves_the_room_free_and_can_be_stopped() {
    use mic_core::Command;
    use mic_core::video::ScreenSource;
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    // A capture that opens only after a while, as one whose desktop asks which screen.
    let screen: ScreenSource = std::sync::Arc::new(|_pictures| {
        std::thread::sleep(Duration::from_secs(3));
        Ok(Running::new(|| {}))
    });
    let alice = Client::new(
        config(&relay),
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
    .join_with(
        invite.clone(),
        Profile::new("Alice"),
        Media {
            screen: Some(screen),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let sharing = tokio::spawn({
        let alice = alice.clone();
        async move { alice.command(Command::ShareScreen { on: true }).await }
    });
    let mut state = alice.state();
    tokio::time::timeout(PATIENCE, state.wait_for(|state| state.screen_opening))
        .await
        .unwrap()
        .unwrap();
    // The room still answers while the capture opens.
    tokio::time::timeout(
        Duration::from_secs(1),
        alice.command(Command::SetHand { raised: true }),
    )
    .await
    .unwrap()
    .unwrap();
    // Stopped while it opens: nothing is shared, and the share's asker hears so at once.
    alice
        .command(Command::ShareScreen { on: false })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), sharing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let state = alice.state().borrow().clone();
    assert!(!state.screen_opening);
    assert!(!state.me.profile.sharing);
}

#[test]
fn under_a_second_isnt_a_recording() {
    let pictures = (0..CAMERA_SETTINGS.fps - 1)
        .map(|_| mic_media::video::Picture::grey(CAMERA_SETTINGS.width, CAMERA_SETTINGS.height));
    assert!(mic_core::video::Recorded::encode(pictures).is_err());
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

#[tokio::test(flavor = "multi_thread")]
async fn a_camera_turned_off_sends_nothing_and_says_so() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&invite, "Alice", Some(1), config(&relay)).await;
    let bob = join(&invite, "Bob", Some(2), config(&relay)).await;
    let alices = alice.room.participant();
    sees(&bob, alices, 1).await;

    alice
        .room
        .command(mic_core::Command::SetCamera { on: false })
        .await
        .unwrap();
    let mut state = bob.room.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| {
            state
                .participants
                .iter()
                .any(|them| them.profile.camera_off)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    // What was on its way has arrived by now; then nothing more comes.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = bob.room.video_count(alices);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        bob.room.video_count(alices),
        before,
        "pictures while it's off"
    );

    alice
        .room
        .command(mic_core::Command::SetCamera { on: true })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while bob.room.video_count(alices) < before + 10 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no pictures once it's on again"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!bob.room.state().borrow().participants[0].profile.camera_off);
}

#[tokio::test(flavor = "multi_thread")]
async fn ones_own_camera_is_previewed_and_none_without_one() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&invite, "Alice", Some(1), config(&relay)).await;
    let dave = join(&invite, "Dave", None, config(&relay)).await;
    assert!(dave.room.preview().is_none());
    let mut preview = alice.room.preview().unwrap();
    let picture = tokio::time::timeout(PATIENCE, async {
        loop {
            preview.changed().await.unwrap();
            if let Some(picture) = preview.borrow_and_update().clone() {
                return picture;
            }
        }
    })
    .await
    .unwrap();
    // Her own pattern, before any codec: it names her.
    assert_eq!(
        read_pattern(&picture).map(|pattern| pattern.sender),
        Some(1)
    );
}
