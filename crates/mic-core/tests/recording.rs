//! Recording through `mic-testrelay`: one who records writes a WebM file of each participant,
//! itself among them, everyone sees that it records, and the files play: their camera in VP9 and
//! their voice in Opus, as ffprobe reads them where it's installed.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::video::{CAMERA_SETTINGS, Camera};
use mic_core::{Client, Command, Config, Invite, Media, Profile, Room, Trust};
use mic_media::audio::{Mixer, Running, Source, ToneSource, capture};
use mic_media::video::PatternCamera;
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(15);

struct Participant {
    room: Room,
    _running: Vec<Running>,
}

async fn join(
    relay: &TestRelay,
    invite: &Invite,
    name: &str,
    sender: u8,
    tone: f64,
    recordings: &std::path::Path,
) -> Participant {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    config.recordings = recordings.to_owned();
    let mut running = Vec::new();
    let (writer, captured) = capture();
    running.push(Box::new(ToneSource::new(tone)).start(writer).unwrap());
    let (pictures, shown) = tokio::sync::watch::channel(None);
    running.push(
        PatternCamera {
            sender,
            width: CAMERA_SETTINGS.width,
            height: CAMERA_SETTINGS.height,
            fps: CAMERA_SETTINGS.fps,
        }
        .start(pictures)
        .unwrap(),
    );
    let room = Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
    .join_with(
        invite.clone(),
        Profile::new(name),
        Media {
            capture: Some(captured),
            mixer: Some(Mixer::new()),
            camera: Some(Camera::Live(shown)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Participant {
        room,
        _running: running,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_who_records_writes_everyone_and_everyone_sees_it() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let recordings = std::env::temp_dir().join(format!("mic-recordings-{}", std::process::id()));
    let alice = join(&relay, &invite, "Alice", 1, 440.0, &recordings).await;
    let bob = join(&relay, &invite, "Bob", 2, 660.0, &recordings).await;
    let mut state = alice.room.state();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.len() == 1),
    )
    .await
    .unwrap()
    .unwrap();
    // What there is to record has arrived.
    tokio::time::sleep(Duration::from_secs(2)).await;
    bob.room
        .command(Command::Record { on: true })
        .await
        .unwrap();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| state.participants.iter().any(|them| them.profile.recording)),
    )
    .await
    .expect("alice sees bob record")
    .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    bob.room
        .command(Command::Record { on: false })
        .await
        .unwrap();
    tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| {
            state
                .participants
                .iter()
                .all(|them| !them.profile.recording)
        }),
    )
    .await
    .expect("alice sees bob stop")
    .unwrap();

    let directories: Vec<_> = std::fs::read_dir(&recordings).unwrap().flatten().collect();
    assert_eq!(directories.len(), 1, "one recording");
    let mut files: Vec<String> = std::fs::read_dir(directories[0].path())
        .unwrap()
        .flatten()
        .map(|file| file.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(
        files[0].starts_with("Alice-") && files[1].starts_with("Bob-"),
        "{files:?}"
    );
    for file in &files {
        let path = directories[0].path().join(file);
        let written = std::fs::read(&path).unwrap();
        assert!(written.len() > 20_000, "{file}: {} bytes", written.len());
        let Ok(probed) = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,nb_read_frames",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
        else {
            continue;
        };
        let said = String::from_utf8_lossy(&probed.stdout);
        assert!(
            probed.status.success(),
            "{file}: {}",
            String::from_utf8_lossy(&probed.stderr)
        );
        // About three seconds: at 15 a second, and 50 packets.
        let frames = |codec: &str| -> u64 {
            said.lines()
                .find_map(|line| line.strip_prefix(&format!("{codec},")))
                .and_then(|count| count.trim().parse().ok())
                .unwrap_or(0)
        };
        assert!(frames("vp9") >= 30, "{file}: {said}");
        assert!(frames("opus") >= 100, "{file}: {said}");
    }
    std::fs::remove_dir_all(&recordings).unwrap();
}
