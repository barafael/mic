//! `mic://` links (PLAN.md, commit 41): what opens them, and the one window they open in.
//!
//! - **Registering.** On Linux, `mic --register` installs a desktop file naming this binary as the
//!   handler of `mic://` (an AppImage can't do that by itself). Windows' registry entry and
//!   macOS's `Info.plist` come with their installers (M10).
//! - **One window.** A link opened while mic runs starts a second mic, which hands the link to the
//!   first, over a socket in the user's runtime directory, and exits: the room opens in the
//!   window already there.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;

/// The longest link a hand-over reads.
#[cfg(unix)]
const LONGEST: u64 = 4096;

/// Where links are handed over: the user's runtime directory, which is theirs alone. On macOS,
/// the user's own temporary directory; elsewhere without a runtime directory, nowhere: a shared
/// `/tmp` would hand an invite's secret to whoever listened there first.
#[cfg(unix)]
fn socket() -> Option<PathBuf> {
    let directory = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(directory) => PathBuf::from(directory),
        None if cfg!(target_os = "macos") => std::env::temp_dir(),
        None => return None,
    };
    Some(directory.join("mic-links"))
}

/// Hands `link` to a mic already running, if there is one; true if it took it.
#[cfg(unix)]
pub fn hand_over(link: &str) -> bool {
    let Some(socket) = socket() else {
        return false;
    };
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return false;
    };
    writeln!(stream, "{link}").is_ok()
}

#[cfg(not(unix))]
pub fn hand_over(_link: &str) -> bool {
    false
}

/// Listens for links other mics hand over, calling `opened` with each, from a thread of its own,
/// for the life of the process. One left by a mic that's gone is replaced; one a mic still
/// listens on is left to it.
#[cfg(unix)]
pub fn listen(opened: impl Fn(String) + Send + 'static) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::net::{UnixListener, UnixStream};
    let path = socket().ok_or_else(|| Error::new(ErrorKind::NotFound, "no $XDG_RUNTIME_DIR"))?;
    if UnixStream::connect(&path).is_ok() {
        return Err(Error::new(
            ErrorKind::AddrInUse,
            format!("another mic opens links ({})", path.display()),
        ));
    }
    let listener = mic_core::local::bind_private(&path, |at| UnixListener::bind(at))?;
    std::thread::Builder::new()
        .name("mic-links".to_owned())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                // One that never says its link doesn't hold up the next.
                if stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .is_err()
                {
                    continue;
                }
                let mut line = String::new();
                if BufReader::new(stream.take(LONGEST))
                    .read_line(&mut line)
                    .is_ok()
                    && !line.trim().is_empty()
                {
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
pub fn register(arguments: &[String]) -> Result<String, String> {
    // An AppImage runs from a mount that's gone once it exits: the image itself is what stays.
    let exe = match std::env::var_os("APPIMAGE") {
        Some(image) => PathBuf::from(image),
        None => std::env::current_exe().map_err(|error| format!("this binary: {error}"))?,
    };
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
    std::fs::write(&desktop, desktop_file(&exe.to_string_lossy(), arguments))
        .map_err(|error| format!("{}: {error}", desktop.display()))?;
    // As xdg-mime would set it, without needing xdg-utils.
    std::fs::create_dir_all(&config).map_err(|error| error.to_string())?;
    let list = config.join("mimeapps.list");
    // Only a list that isn't there yet starts empty: one that can't be read isn't overwritten.
    let before = match std::fs::read_to_string(&list) {
        Ok(before) => before,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{}: {error}", list.display())),
    };
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
pub fn register(_arguments: &[String]) -> Result<String, String> {
    Err("--register is for Linux; Windows and macOS register with their installers".to_owned())
}

/// `argument` as the desktop file's `Exec` takes it: quoted only if it must be (a character the
/// spec reserves), since xdg-open's own reading of `Exec` takes a quoted path's quotes for part
/// of it, and finds no such command. Inside quotes, `"`, `` ` ``, `$` and `\` are escaped, and
/// that escape's backslash is escaped again, as a string value's; `%` is doubled either way.
fn exec_argument(argument: &str) -> String {
    let plain = argument
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:=,@%".contains(c));
    let percent = argument.replace('%', "%%");
    if plain {
        return percent;
    }
    let mut quoted = String::from("\"");
    for c in percent.chars() {
        match c {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '\\' => quoted.push_str("\\\\\\\\"),
            '\n' => quoted.push_str("\\n"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// The desktop file for `exe`, given `arguments` before the link: an application that opens
/// `mic://` links.
fn desktop_file(exe: &str, arguments: &[String]) -> String {
    let exec = std::iter::once(exe)
        .chain(arguments.iter().map(String::as_str))
        .map(exec_argument)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Desktop Entry]\nType=Application\nName=mic\nComment=Video calls, end-to-end encrypted\n\
         Exec={exec} %u\nTerminal=false\nMimeType=x-scheme-handler/mic;\n\
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
        let file = desktop_file("/opt/my mic/mic", &[]);
        assert!(file.contains("Exec=\"/opt/my mic/mic\" %u\n"));
        assert!(desktop_file("/opt/mic/mic", &[]).contains("Exec=/opt/mic/mic %u\n"));
        assert!(file.contains("MimeType=x-scheme-handler/mic;\n"));
        let relay = [
            "--relay".to_owned(),
            "https://relay.example:4443/".to_owned(),
        ];
        assert!(
            desktop_file("/opt/mic/mic", &relay)
                .contains("Exec=/opt/mic/mic --relay https://relay.example:4443/ %u\n")
        );
    }

    #[test]
    fn what_exec_reserves_is_escaped() {
        assert_eq!(exec_argument("/opt/100%/mic"), "/opt/100%%/mic");
        assert_eq!(exec_argument("/opt/$x/mic"), "\"/opt/\\\\$x/mic\"");
        assert_eq!(exec_argument("/opt/a\\b"), "\"/opt/a\\\\\\\\b\"");
    }
}
