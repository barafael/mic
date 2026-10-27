//! Driving a client's window as a person would, with xdotool on its display: keys typed into it,
//! and clicks on it. Xvnc has no window manager, so the window is given the keyboard first, and
//! sits at the display's top left at its preferred size (crates/mic-slint/ui/app.slint), which is
//! where clicks are aimed.

use mic_core::Invite;

use crate::error::{Error, Result};
use crate::podman::TestBox;
use crate::view;

/// The window's size, as Slint gives it with no window manager: its preferred one.
const WINDOW: (u32, u32) = (960, 600);
/// Where the room screen's Mute button is: in the top row, left of Leave.
const MUTE: (u32, u32) = (WINDOW.0 - 104, 23);

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

/// Clicks the room screen's Mute button in `client`'s window.
pub fn click_mute(the_box: &TestBox, client: &'static str) -> Result<()> {
    let (x, y) = (MUTE.0.to_string(), MUTE.1.to_string());
    xdotool(the_box, client, &[&["mousemove", &x, &y], &["click", "1"]])
}
