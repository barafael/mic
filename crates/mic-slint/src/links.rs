//! `mic://` links (PLAN.md, commit 41): what opens them, and the one window they open in.
//!
//! - **Registering.** On Linux, `mic --register` installs a desktop file naming this binary as the
//!   handler of `mic://` (an AppImage can't do that by itself). Windows' registry entry and
//!   macOS's `Info.plist` come with their installers (M10).
//! - **One window.** A link opened while mic runs starts a second mic, which hands the link to the
//!   first, over a socket in the user's runtime directory, and exits: the room opens in the
//!   window already there.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// What a link handed over is: one line, the link.
#[cfg(unix)]
fn socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("mic-links")
}

/// Hands `link` to a mic already running, if there is one; true if it took it.
#[cfg(unix)]
pub fn hand_over(link: &str) -> bool {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket()) else {
        return false;
    };
    writeln!(stream, "{link}").is_ok()
}

#[cfg(not(unix))]
pub fn hand_over(_link: &str) -> bool {
    false
}

/// Listens for links other mics hand over, calling `opened` with each, from a thread of its own,
/// for the life of the process. One left by a mic that's gone is replaced.
#[cfg(unix)]
pub fn listen(opened: impl Fn(String) + Send + 'static) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = socket();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("mic-links".to_owned())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() && !line.trim().is_empty() {
                    opened(line.trim().to_owned());
                }
            }
        })?;
    Ok(())
}

#[cfg(not(unix))]
pub fn listen(_opened: impl Fn(String) + Send + 'static) -> std::io::Result<()> {
    Ok(())
}

/// Installs this binary as the handler of `mic://` links, for this user: a desktop file, made the
/// default for the scheme. Says what it did.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn register() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|error| format!("this binary: {error}"))?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("no $HOME")?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let applications = data.join("applications");
    std::fs::create_dir_all(&applications).map_err(|error| error.to_string())?;
    let desktop = applications.join("mic.desktop");
    std::fs::write(&desktop, desktop_file(&exe.to_string_lossy()))
        .map_err(|error| format!("{}: {error}", desktop.display()))?;
    // As xdg-mime would set it, without needing xdg-utils.
    std::fs::create_dir_all(&config).map_err(|error| error.to_string())?;
    let list = config.join("mimeapps.list");
    let before = std::fs::read_to_string(&list).unwrap_or_default();
    std::fs::write(&list, with_default(&before))
        .map_err(|error| format!("{}: {error}", list.display()))?;
    Ok(format!(
        "{} opens mic:// links now: {}, {}",
        exe.display(),
        desktop.display(),
        list.display()
    ))
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
pub fn register() -> Result<String, String> {
    Err("--register is for Linux; Windows and macOS register with their installers".to_owned())
}

/// The desktop file for `exe`: an application that opens `mic://` links. Its path is quoted only
/// if it must be (a space, or a character the spec reserves): xdg-open's own reading of `Exec`
/// takes a quoted path's quotes for part of it, and finds no such command.
fn desktop_file(exe: &str) -> String {
    let plain = exe
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c));
    let quoted = if plain {
        exe.to_owned()
    } else {
        format!("\"{}\"", exe.replace('\\', "\\\\").replace('"', "\\\""))
    };
    format!(
        "[Desktop Entry]\nType=Application\nName=mic\nComment=Video calls, end-to-end encrypted\n\
         Exec={quoted} %u\nTerminal=false\nMimeType=x-scheme-handler/mic;\n\
         Categories=Network;VideoConference;\n"
    )
}

/// `mimeapps.list` as `list` was, with mic the default for `mic://`: its other lines as they were.
fn with_default(list: &str) -> String {
    const SECTION: &str = "[Default Applications]";
    const ENTRY: &str = "x-scheme-handler/mic=mic.desktop";
    let mut lines: Vec<String> = list
        .lines()
        .filter(|line| !line.starts_with("x-scheme-handler/mic="))
        .map(str::to_owned)
        .collect();
    match lines.iter().position(|line| line.trim() == SECTION) {
        Some(at) => lines.insert(at + 1, ENTRY.to_owned()),
        None => {
            if lines.last().is_some_and(|line| !line.is_empty()) {
                lines.push(String::new());
            }
            lines.push(SECTION.to_owned());
            lines.push(ENTRY.to_owned());
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_becomes_the_default_and_the_rest_stays() {
        let before = "[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/mic=old.desktop\n\n[Added Associations]\nimage/png=gimp.desktop\n";
        let after = with_default(before);
        assert!(after.contains("[Default Applications]\nx-scheme-handler/mic=mic.desktop\n"));
        assert!(after.contains("text/html=firefox.desktop"));
        assert!(after.contains("image/png=gimp.desktop"));
        assert!(!after.contains("old.desktop"));
        assert_eq!(
            with_default(""),
            "[Default Applications]\nx-scheme-handler/mic=mic.desktop\n"
        );
    }

    #[test]
    fn the_desktop_file_opens_links_with_this_binary() {
        let file = desktop_file("/opt/my mic/mic");
        assert!(file.contains("Exec=\"/opt/my mic/mic\" %u\n"));
        assert!(desktop_file("/opt/mic/mic").contains("Exec=/opt/mic/mic %u\n"));
        assert!(file.contains("MimeType=x-scheme-handler/mic;\n"));
    }
}
