//! Recording through `mic-testrelay`: one who records writes the call into a WebM file, and
//! everyone sees that it records. Where ffmpeg is installed, the file plays, VP9 and Opus, and
//! while only alice speaks it shows her camera.

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
    // Only alice speaks.
    bob.room
        .command(Command::SetMuted { muted: true })
        .await
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

    let files: Vec<_> = std::fs::read_dir(&recordings).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "one recording");
    let path = files[0].path();
    let written = std::fs::read(&path).unwrap();
    assert!(written.len() > 20_000, "{} bytes", written.len());
    let Ok(probed) = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            "stream=codec_name,width,nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
    else {
        std::fs::remove_dir_all(&recordings).unwrap();
        return;
    };
    let said = String::from_utf8_lossy(&probed.stdout);
    assert!(
        probed.status.success(),
        "{}",
        String::from_utf8_lossy(&probed.stderr)
    );
    // About three seconds: 15 pictures a second, and 50 packets.
    let frames = |codec: &str| -> u64 {
        said.lines()
            .find(|line| line.starts_with(&format!("{codec},")))
            .and_then(|line| line.rsplit(',').next())
            .and_then(|count| count.trim().parse().ok())
            .unwrap_or(0)
    };
    assert!(said.contains("vp9,1280"), "{said}");
    assert!(frames("vp9") >= 30, "{said}");
    assert!(frames("opus") >= 100, "{said}");
    // Two seconds in, only alice has spoken: it shows her camera.
    let frame = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss", "2", "-i"])
        .arg(&path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .output()
        .unwrap();
    let picture = mic_media::video::Picture {
        width: 1280,
        height: 720,
        data: frame.stdout,
    };
    let read = mic_media::video::read_pattern(&picture);
    assert_eq!(read.map(|pattern| pattern.sender), Some(1), "{read:?}");
    std::fs::remove_dir_all(&recordings).unwrap();
}
