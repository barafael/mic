//! The box's images, tagged by what they're built from: a hash of the Containerfile, every file
//! of the build context and the pins the image takes. `up` builds an image only when no image has
//! its tag yet, so an unchanged box starts without running `podman build`.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{self, Context, Result};
use crate::pins::{self, Pin};
use crate::podman::podman;
use crate::process;

/// What a tag's hash is cut to: 16 hex digits, 64 bits, leave no room for a chance collision
/// between two states of one image.
const HASH_DIGITS: usize = 16;

/// An image of the box, built from `testbox/<name>/`: its Containerfile and nothing else that
/// the build doesn't need, since every file there is in its tag.
pub struct Image {
    /// The image's directory under `testbox/`, and its name in pod files (`@IMAGE:<name>@`).
    pub name: &'static str,
    /// The build arguments it takes.
    pub pins: &'static [Pin],
}

pub const IMAGES: [Image; 1] = [
    // moq's relay, and its clock example as a stock publisher and subscriber.
    Image {
        name: "moq",
        pins: &[pins::ARCH, pins::ARCH_SNAPSHOT, pins::MOQ_REV],
    },
];

pub fn find(name: &str) -> Option<&'static Image> {
    IMAGES.iter().find(|image| image.name == name)
}

impl Image {
    /// The build context: the image's directory, with its Containerfile.
    pub fn context(&self, repo: &Path) -> PathBuf {
        repo.join("testbox").join(self.name)
    }

    /// `localhost/mic-testbox-<name>:<hash>`.
    pub fn tag(&self, repo: &Path) -> Result<String> {
        Ok(format!(
            "localhost/mic-testbox-{}:{}",
            self.name,
            self.hash(repo)?
        ))
    }

    /// The hash of everything the image is built from. Each file goes in with its path, so a
    /// renamed file changes it too.
    fn hash(&self, repo: &Path) -> Result<String> {
        let context = self.context(repo);
        let mut hasher = Sha256::new();
        for file in files(&context)? {
            let relative = file.strip_prefix(&context).unwrap_or(&file);
            let bytes = error::read(&file)?;
            hasher.update(relative.to_string_lossy().as_bytes());
            hasher.update([0]);
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(&bytes);
        }
        for pin in self.pins {
            hasher.update(pin.name.as_bytes());
            hasher.update([0]);
            hasher.update(pin.value.as_bytes());
            hasher.update([0]);
        }
        let digest = hasher.finalize();
        Ok(digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()[..HASH_DIGITS]
            .to_owned())
    }

    /// Builds the image unless one with its tag exists; returns the tag, and whether it built.
    pub fn ensure(&self, repo: &Path) -> Result<(String, bool)> {
        let tag = self.tag(repo)?;
        if process::succeeds(podman(&["image", "exists", &tag]))? {
            return Ok((tag, false));
        }
        let mut build = podman(&["build", "--jobs", "0", "--tag", &tag]);
        for pin in self.pins {
            build
                .arg("--build-arg")
                .arg(format!("{}={}", pin.name, pin.value));
        }
        build.arg(self.context(repo));
        process::run(build)?;
        Ok((tag, true))
    }
}

/// Every file under `directory`, in a fixed order.
fn files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .context(directory.display().to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()
        .context(directory.display().to_string())?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            found.extend(files(&path)?);
        } else {
            found.push(path);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context directory of its own under the test's target directory.
    fn context(name: &str) -> PathBuf {
        let repo = std::env::temp_dir().join(format!("mic-xtask-{}-{name}", std::process::id()));
        if repo.exists() {
            std::fs::remove_dir_all(&repo).unwrap();
        }
        std::fs::create_dir_all(repo.join("testbox/tools/sub")).unwrap();
        std::fs::write(repo.join("testbox/tools/Containerfile"), "FROM x\n").unwrap();
        std::fs::write(repo.join("testbox/tools/sub/file"), "a").unwrap();
        repo
    }

    const IMAGE: Image = Image {
        name: "tools",
        pins: &[pins::ARCH],
    };

    #[test]
    fn the_tag_is_the_same_for_the_same_inputs() {
        let repo = context("same");
        assert_eq!(IMAGE.tag(&repo).unwrap(), IMAGE.tag(&repo).unwrap());
        assert!(
            IMAGE
                .tag(&repo)
                .unwrap()
                .starts_with("localhost/mic-testbox-tools:")
        );
    }

    #[test]
    fn the_tag_changes_with_a_file_its_name_or_a_pin() {
        let repo = context("changes");
        let before = IMAGE.tag(&repo).unwrap();

        std::fs::write(repo.join("testbox/tools/sub/file"), "b").unwrap();
        let edited = IMAGE.tag(&repo).unwrap();
        assert_ne!(before, edited);

        std::fs::rename(
            repo.join("testbox/tools/sub/file"),
            repo.join("testbox/tools/sub/other"),
        )
        .unwrap();
        let renamed = IMAGE.tag(&repo).unwrap();
        assert_ne!(edited, renamed);

        let bumped = Image {
            name: "tools",
            pins: &[Pin {
                name: "ARCH",
                value: "another",
            }],
        };
        assert_ne!(renamed, bumped.tag(&repo).unwrap());
    }
}
