//! Chat, reactions and raised hands through `mic-testrelay`: what one says, everyone sees, once.

use std::time::Duration;

use mic_core::chat::Reaction;
use mic_core::secrets::InMemory;
use mic_core::{Client, Command, Config, Invite, Profile, Room, RoomState, Trust};
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

async fn join(relay: &TestRelay, invite: &Invite, name: &str) -> Room {
    client(relay)
        .join(invite.clone(), Profile::new(name))
        .await
        .unwrap()
}

/// Waits until `room`'s state holds, within the patience.
async fn until(room: &Room, what: &str, holds: impl Fn(&RoomState) -> bool) {
    let mut state = room.state();
    let held = tokio::time::timeout(PATIENCE, state.wait_for(|state| holds(state)))
        .await
        .is_ok();
    assert!(held, "{what}: {:?}", *room.state().borrow());
}

#[tokio::test(flavor = "multi_thread")]
async fn what_one_says_everyone_sees_once() {
    let relay = TestRelay::start().await.unwrap();
    let invite = Invite::new_room();
    let alice = join(&relay, &invite, "Alice").await;
    let bob = join(&relay, &invite, "Bob").await;
    let carol = join(&relay, &invite, "Carol").await;
    for room in [&alice, &bob, &carol] {
        until(room, "everyone in", |state| state.participants.len() == 2).await;
    }
    alice
        .command(Command::Say {
            text: "  hello, room  ".to_owned(),
        })
        .await
        .unwrap();
    bob.command(Command::React {
        reaction: Reaction::Clap,
    })
    .await
    .unwrap();
    carol
        .command(Command::SetHand { raised: true })
        .await
        .unwrap();
    // Nothing to say says nothing.
    carol
        .command(Command::Say {
            text: "   ".to_owned(),
        })
        .await
        .unwrap();
    let (alices, bobs, carols) = (alice.participant(), bob.participant(), carol.participant());
    for room in [&alice, &bob, &carol] {
        until(room, "alice's message, once", |state| {
            state.chat.len() == 1
                && state.chat[0].from == alices
                && state.chat[0].name == "Alice"
                && state.chat[0].text == "hello, room"
        })
        .await;
        until(room, "bob's clap, once", |state| {
            state.reactions.len() == 1
                && state.reactions[0].from == bobs
                && state.reactions[0].reaction == Reaction::Clap
        })
        .await;
        until(room, "carol's hand up", |state| {
            std::iter::once(&state.me)
                .chain(&state.participants)
                .any(|participant| participant.key == carols && participant.profile.hand)
        })
        .await;
    }
    // A second, and the same reaction again: each comes, in order.
    alice
        .command(Command::Say {
            text: "again".to_owned(),
        })
        .await
        .unwrap();
    bob.command(Command::React {
        reaction: Reaction::Clap,
    })
    .await
    .unwrap();
    until(&carol, "both messages and both claps", |state| {
        state
            .chat
            .iter()
            .map(|message| message.text.as_str())
            .eq(["hello, room", "again"])
            && state.reactions.len() == 2
            && state.reactions[0].seq != state.reactions[1].seq
    })
    .await;
    // Held a moment: nothing comes twice.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let state = carol.state().borrow().clone();
    assert_eq!((state.chat.len(), state.reactions.len()), (2, 2));
}
