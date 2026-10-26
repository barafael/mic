//! The test relay relays: a publisher and a subscriber, each a moq-tokio client of its own, pass
//! frames through it, and do again after it restarts.

use std::time::Duration;

use mic_testrelay::TestRelay;
use moq_net::Timestamp;

/// How long anything here may take: everything is on loopback.
const PATIENCE: Duration = Duration::from_secs(10);

/// What keeps a publisher publishing: its connection, its broadcast and its track. Dropping the
/// track's producer ends the track.
type Publisher = (
    moq_tokio::connection::Connection,
    moq_net::broadcast::Producer,
    moq_net::track::Producer,
);

/// Publishes the track `frames` of `broadcast` through `relay`, with one group of `frames`, for
/// as long as what's returned lives.
fn publish(relay: &TestRelay, broadcast: &str, frames: &[&str]) -> anyhow::Result<Publisher> {
    let origin = moq_tokio::origin::spawn();
    let producer = origin.create_broadcast(broadcast)?;
    let track = producer.create_track("frames", None)?;
    let mut group = track.create_group(0u64.into())?;
    for frame in frames {
        group.write_frame(Timestamp::now(), frame.to_string())?;
    }
    group.finish()?;
    producer.announce(Default::default())?;
    let client = relay.connect_config().init(Default::default())?;
    let connection = client.with_publisher(&origin).connect(relay.url());
    Ok((connection, producer, track))
}

/// What a subscriber to `broadcast` through `relay` reads of its first group.
async fn subscribe(relay: &TestRelay, broadcast: &str) -> anyhow::Result<Vec<String>> {
    let origin = moq_tokio::origin::spawn();
    let client = relay.connect_config().init(Default::default())?;
    let _connection = client.with_subscriber(origin.clone()).connect(relay.url());
    let consumer = origin.consume();
    // Asked for only once the relay has announced it: before, it's unroutable.
    let mut announced = consumer.announced();
    loop {
        let update = announced
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("announcements ended"))?;
        if update.kind.is_active() && update.prefix.as_str() == broadcast {
            break;
        }
    }
    let broadcast = consumer.request_broadcast(broadcast).await?;
    let mut track = broadcast.track("frames")?.subscribe(None).await?;
    let mut group = track
        .recv_group()
        .await?
        .ok_or_else(|| anyhow::anyhow!("the track ended without a group"))?;
    let mut read = Vec::new();
    while let Some(frame) = group.read_frame().await? {
        read.push(String::from_utf8_lossy(&frame.payload).into_owned());
    }
    Ok(read)
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_pass_through_the_relay_and_again_after_a_restart() {
    let mut relay = TestRelay::start().await.unwrap();
    let _publisher = publish(&relay, "before", &["one", "two"]).unwrap();
    let read = tokio::time::timeout(PATIENCE, subscribe(&relay, "before"))
        .await
        .expect("in time")
        .unwrap();
    assert_eq!(read, ["one", "two"]);

    let fingerprint = relay.fingerprint().to_owned();
    relay.restart().await.unwrap();
    assert_eq!(relay.fingerprint(), fingerprint, "the same certificate");
    let _publisher = publish(&relay, "after", &["three"]).unwrap();
    let read = tokio::time::timeout(PATIENCE, subscribe(&relay, "after"))
        .await
        .expect("in time")
        .unwrap();
    assert_eq!(read, ["three"]);
}
