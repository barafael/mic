//! Driving a client's window as a person would, with xdotool on its display: keys typed into it,
//! and its shortcuts. Xvnc has no window manager, so the window is given the keyboard first. No
//! clicks at coordinates: a button that moves when another arrives beside it took one to the
//! wrong button once (docs/observations.md).

use mic_core::Invite;

use crate::error::{Error, Result};
use crate::podman::TestBox;
use crate::view;

/// Runs xdotool on `client`'s display, one invocation a step, after the window gets the
/// keyboard. One invocation each, since `type` takes every argument after it as text.
fn xdotool(the_box: &TestBox, client: &'static str, steps: &[&[&str]]) -> Result<()> {
    let focus: &[&str] = &[
        "search",
        "--sync",
        "--name",
        "^mic$",
        "windowfocus",
        "--sync",
    ];
    for step in std::iter::once(focus).chain(steps.iter().copied()) {
        let command: Vec<&str> = ["env", "DISPLAY=:1", "xdotool"]
            .into_iter()
            .chain(step.iter().copied())
            .collect();
        let ran = the_box.exec(&view::container(client), &command)?;
        if !ran.status.success() {
            return Err(Error::Failed(format!(
                "xdotool {step:?} in {client}: {}",
                String::from_utf8_lossy(&ran.stderr).trim()
            )));
        }
    }
    Ok(())
}

/// Types `name` and `invite` into `client`'s join screen and presses Enter: the name field has
/// the focus when the screen shows, Tab moves to the invite's, and each is emptied first.
pub fn type_join(
    the_box: &TestBox,
    client: &'static str,
    name: &str,
    invite: &Invite,
) -> Result<()> {
    let invite = invite.to_mic_url();
    xdotool(
        the_box,
        client,
        &[
            &["key", "ctrl+a"],
            &["type", "--delay", "10", name],
            &["key", "Tab", "ctrl+a"],
            &["type", "--delay", "2", &invite],
            &["key", "Return"],
        ],
    )
}

/// Presses the room screen's shortcut for the microphone, Ctrl+D, in `client`'s window: what
/// its Mute button does (crates/mic-slint/ui/app.slint, `shortcuts`).
pub fn toggle_mute(the_box: &TestBox, client: &'static str) -> Result<()> {
    xdotool(the_box, client, &[&["key", "ctrl+d"]])
}

/// Presses `key` in `client`'s window, as xdotool names it: `Return`, `Escape`, …
pub fn press(the_box: &TestBox, client: &'static str, key: &str) -> Result<()> {
    xdotool(the_box, client, &[&["key", key]])
}

/// Opens the chat with its shortcut, Ctrl+Alt+C, which gives its message field the keyboard,
/// types `text` there, and sends it with Enter.
pub fn type_message(the_box: &TestBox, client: &'static str, text: &str) -> Result<()> {
    xdotool(
        the_box,
        client,
        &[
            &["key", "ctrl+alt+c"],
            &["type", "--delay", "10", text],
            &["key", "Return"],
        ],
    )
}
