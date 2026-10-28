//! Sockets for the user's own processes: the box's harness, and the hand-over of `mic://` links
//! to a running mic.

#[cfg(unix)]
use std::path::Path;

/// A listener at `path`, bound by `bind`, never reachable by anyone but the owner: bound inside a
/// directory of its own, the owner's only, made private there, then moved into place. Binding at
/// `path` and restricting it after would leave a moment in which anyone who can reach the
/// directory could connect. The move replaces a socket left at `path` from before.
#[cfg(unix)]
pub fn bind_private<L>(
    path: &Path,
    bind: impl FnOnce(&Path) -> std::io::Result<L>,
) -> std::io::Result<L> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let parent = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staging = parent.join(format!(".{name}.{}.binding", std::process::id()));
    // One left by a former process of this id, which can only be ours.
    match std::fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let staged = staging.join("socket");
    let bound = bind(&staged).and_then(|listener| {
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&staged, path)?;
        Ok(listener)
    });
    let removed = std::fs::remove_dir_all(&staging);
    let listener = bound?;
    removed?;
    Ok(listener)
}
