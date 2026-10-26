//! Who is in the room, through `mic-testrelay`.

use std::time::Duration;

use mic_core::secrets::InMemory;
use mic_core::{Client, Command, Config, Invite, Participant, Profile, Room, Trust};
use mic_testrelay::TestRelay;

const PATIENCE: Duration = Duration::from_secs(10);

fn client(relay: &TestRelay) -> Client {
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    Client::new(
        config,
        &InMemory::default(),
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

/// Waits until the others in `room` are `names`, in order; fails with who they were.
async fn roster_is(room: &Room, names: &[&str]) {
    let mut state = room.state();
    let names_of = |participants: &[Participant]| -> Vec<String> {
        participants
            .iter()
            .map(|participant| participant.profile.name.clone())
            .collect()
    };
    let reached = tokio::time::timeout(
        PATIENCE,
        state.wait_for(|state| names_of(&state.participants) == names),
    )
    .await;
    if reached.is_err() {
        let state = room.state().borrow().clone();
        panic!(
            "{}'s roster: {:?}, not {names:?}",
            state.me.profile.name,
            names_of(&state.participants)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn participants_see_each_other_come_and_go() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let bob = client(&relay)
        .join(invite.clone(), Profile::new("Bob"))
        .await
        .unwrap();
    roster_is(&alice, &["Bob"]).await;
    roster_is(&bob, &["Alice"]).await;
    let bobs = alice.state().borrow().participants[0].key;
    assert_eq!(bobs, bob.participant());

    let carol = client(&relay)
        .join(invite, Profile::new("Carol"))
        .await
        .unwrap();
    roster_is(&alice, &["Bob", "Carol"]).await;
    roster_is(&carol, &["Alice", "Bob"]).await;

    bob.command(Command::Leave).await.unwrap();
    roster_is(&alice, &["Carol"]).await;
    roster_is(&carol, &["Alice"]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_name_reaches_the_others() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = client(&relay)
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let bob = client(&relay)
        .join(invite, Profile::new("Bob"))
        .await
        .unwrap();
    roster_is(&bob, &["Alice"]).await;
    alice
        .command(Command::SetName {
            name: "Alice B.".to_owned(),
        })
        .await
        .unwrap();
    roster_is(&bob, &["Alice B."]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejoin_is_one_participant() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let device = InMemory::default();
    let mut config = Config::new(relay.url());
    config.trust = Trust::Fingerprints(vec![relay.fingerprint().to_owned()]);
    let alices = Client::new(config, &device, tokio::runtime::Handle::current()).unwrap();
    let alice = alices
        .join(invite.clone(), Profile::new("Alice"))
        .await
        .unwrap();
    let bob = client(&relay)
        .join(invite.clone(), Profile::new("Bob"))
        .await
        .unwrap();
    roster_is(&bob, &["Alice"]).await;
    // A second session of the same device in the room, before the first is gone: one Alice.
    let again = alices.join(invite, Profile::new("Alice")).await.unwrap();
    assert_eq!(again.participant(), alice.participant());
    roster_is(&bob, &["Alice"]).await;
    drop(alice);
    roster_is(&bob, &["Alice"]).await;
}
