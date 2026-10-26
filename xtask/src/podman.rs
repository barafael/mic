//! The box under Podman: a pod per participant or service, each from `testbox/pods/<pod>.yaml`
//! under `podman kube play`, all on one internal network. The pod files are apart from the
//! images' build contexts, so editing one rebuilds nothing.
//!
//! A pod per participant, not one pod for the box, because all containers of a pod share one
//! network namespace: network shaping in one would hit everyone (PLAN.md, "A pod per client").

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{self, Error, Result};
use crate::images;
use crate::process;

/// The box's network: `--internal`, so nothing in it reaches the host's network or beyond.
pub const NETWORK: &str = "mic-testbox";
/// What every pod's name starts with; kube play names each container `<pod>-<container>`.
pub const PREFIX: &str = "mic-testbox-";
/// What each phase leaves for looking into it (`checks::Record`).
pub const RECORD: &str = "target/testbox-record";
/// The pod files with their images and paths filled in, which `kube down` needs again.
const RENDERED: &str = "target/testbox-pods";

/// A pod of the box, from `testbox/pods/<name>.yaml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pod {
    /// moq-relay, with the box's TLS.
    Relay,
    /// The checks' tools inside the network.
    Tools,
}

impl Pod {
    /// In the order they're played: the relay makes the keys the others read.
    pub const ALL: [Pod; 2] = [Pod::Relay, Pod::Tools];

    pub fn name(self) -> &'static str {
        match self {
            Pod::Relay => "relay",
            Pod::Tools => "tools",
        }
    }

    pub fn file(self, repo: &Path) -> PathBuf {
        repo.join("testbox/pods")
            .join(format!("{}.yaml", self.name()))
    }

    /// Its name under Podman.
    pub fn podman_name(self) -> String {
        format!("{PREFIX}{}", self.name())
    }

    /// The container `container` of this pod, as kube play names it.
    pub fn container(self, container: &str) -> String {
        format!("{}-{container}", self.podman_name())
    }
}

pub fn podman(args: &[&str]) -> Command {
    let mut podman = Command::new("podman");
    podman.args(args);
    podman
}

pub struct TestBox {
    repo: PathBuf,
}

impl TestBox {
    pub fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_owned(),
        }
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// Builds the images that are missing, makes the network and plays every pod.
    pub fn up(&self) -> Result<()> {
        self.images()?;
        self.network()?;
        let rendered = self.repo.join(RENDERED);
        error::create_dir_all(&rendered)?;
        for pod in Pod::ALL {
            let file = rendered.join(format!("{}.yaml", pod.name()));
            error::write(&file, self.render(pod)?)?;
            let mut play = podman(&["kube", "play", "--replace", "--network", NETWORK]);
            play.arg(&file);
            process::run(play)?;
        }
        Ok(())
    }

    /// Builds each image whose tag is missing; returns those it built.
    pub fn images(&self) -> Result<Vec<&'static str>> {
        let mut built = Vec::new();
        for image in &images::IMAGES {
            let (tag, fresh) = image.ensure(&self.repo)?;
            if fresh {
                println!("built {tag}");
                built.push(image.name);
            }
        }
        Ok(built)
    }

    /// Prints each image's current tag and whether it's built; `prune` removes the box's images
    /// whose tag is no longer any image's current one.
    pub fn list_images(&self, prune: bool) -> Result<()> {
        let mut current = Vec::new();
        for image in &images::IMAGES {
            let tag = image.tag(&self.repo)?;
            let built = process::succeeds(podman(&["image", "exists", &tag]))?;
            println!("{tag}{}", if built { "" } else { " (not built)" });
            current.push(tag);
        }
        if !prune {
            return Ok(());
        }
        let listed = process::output(podman(&[
            "images",
            "--filter",
            "reference=localhost/mic-testbox-*",
            "--format",
            "{{.Repository}}:{{.Tag}}",
        ]))?;
        for stale in listed
            .lines()
            .filter(|tag| !current.iter().any(|kept| kept == tag))
        {
            process::run(podman(&["rmi", stale]))?;
        }
        Ok(())
    }

    /// The network, internal; one of that name that was already there is kept only if it is.
    fn network(&self) -> Result<()> {
        process::run(podman(&[
            "network",
            "create",
            "--ignore",
            "--internal",
            NETWORK,
        ]))?;
        if self.network_is_internal()? {
            Ok(())
        } else {
            Err(Error::Failed(format!(
                "the network {NETWORK} has egress: `podman network rm {NETWORK}`"
            )))
        }
    }

    pub fn network_is_internal(&self) -> Result<bool> {
        let internal = process::output(podman(&[
            "network",
            "inspect",
            "--format",
            "{{.Internal}}",
            NETWORK,
        ]))?;
        Ok(internal.trim() == "true")
    }

    /// `testbox/pods/<pod>.yaml` with `@REPO@` and each `@IMAGE:<name>@` filled in.
    fn render(&self, pod: Pod) -> Result<String> {
        let source = pod.file(&self.repo);
        render(&error::read_to_string(&source)?, &self.repo, |name| {
            images::find(name)
                .ok_or_else(|| Error::Failed(format!("{}: no image {name}", source.display())))?
                .tag(&self.repo)
        })
    }

    /// Stops and removes the pods; `wipe` also removes their volumes. A pod never played from
    /// here is left alone: `kube down` needs the file it was played from.
    pub fn down(&self, wipe: bool) -> Result<()> {
        for pod in Pod::ALL {
            let file = self
                .repo
                .join(RENDERED)
                .join(format!("{}.yaml", pod.name()));
            if !file.exists() {
                continue;
            }
            let mut down = podman(&["kube", "down"]);
            if wipe {
                down.arg("--force");
            }
            down.arg(&file);
            process::run(down)?;
        }
        Ok(())
    }

    /// Every container of the box's pods by name, stopped ones too, but not the pods' infra
    /// containers; none if podman can't say.
    pub fn containers(&self) -> Vec<String> {
        let listed = process::output(podman(&[
            "ps",
            "--all",
            "--filter",
            &format!("name=^{PREFIX}"),
            "--format",
            "{{.Names}} {{.IsInfra}}",
        ]));
        listed
            .map(|lines| {
                lines
                    .lines()
                    .filter_map(|line| line.strip_suffix(" false"))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The container `name`'s log so far, stdout and stderr alike.
    pub fn log(&self, name: &str) -> Result<String> {
        let logs = podman(&["logs", name])
            .output()
            .map_err(|source| Error::Io {
                what: format!("podman logs {name}"),
                source,
            })?;
        if !logs.status.success() {
            return Err(Error::Failed(format!(
                "podman logs {name}: {}",
                String::from_utf8_lossy(&logs.stderr)
            )));
        }
        let mut log = String::from_utf8_lossy(&logs.stdout).into_owned();
        log.push_str(&String::from_utf8_lossy(&logs.stderr));
        Ok(log)
    }

    /// Prints the logs of the containers named, or of every container of the pods named.
    pub fn logs(&self, names: &[String]) -> Result<()> {
        let containers = self.containers();
        for name in names {
            let full = format!("{PREFIX}{name}");
            let matching: Vec<&String> = containers
                .iter()
                .filter(|container| {
                    **container == full || container.starts_with(&format!("{full}-"))
                })
                .collect();
            if matching.is_empty() {
                return Err(Error::Failed(format!(
                    "no container or pod {name} in the box"
                )));
            }
            for container in matching {
                println!("== {container}");
                process::run(podman(&["logs", container]))?;
            }
        }
        Ok(())
    }

    /// Runs `argv` in `container` without a shell, and returns how it ended, whatever that was.
    pub fn exec(&self, container: &str, argv: &[&str]) -> Result<std::process::Output> {
        let mut exec = podman(&["exec", container]);
        exec.args(argv);
        exec.output().map_err(|source| Error::Io {
            what: process::describe(&exec),
            source,
        })
    }

    /// How often `container` was restarted, and the code it last exited with.
    pub fn restarts(&self, container: &str) -> Result<(u32, i32)> {
        let state = process::output(podman(&[
            "container",
            "inspect",
            "--format",
            "{{.RestartCount}} {{.State.ExitCode}}",
            container,
        ]))?;
        let mut words = state.split_whitespace().map(str::parse::<i64>);
        match (words.next(), words.next()) {
            (Some(Ok(restarts)), Some(Ok(code))) => Ok((
                u32::try_from(restarts).unwrap_or(u32::MAX),
                i32::try_from(code).unwrap_or(i32::MAX),
            )),
            _ => Err(Error::Failed(format!(
                "podman inspect {container}: {state:?}"
            ))),
        }
    }
}

/// `source` with `@REPO@` replaced by `repo`, and each `@IMAGE:<name>@` by `tag(name)`.
fn render(source: &str, repo: &Path, tag: impl Fn(&str) -> Result<String>) -> Result<String> {
    let mut rendered = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find("@IMAGE:") {
        rendered.push_str(&rest[..at]);
        let after = &rest[at + "@IMAGE:".len()..];
        let end = after
            .find('@')
            .ok_or_else(|| Error::Failed("an @IMAGE: without its closing @".to_owned()))?;
        rendered.push_str(&tag(&after[..end])?);
        rest = &after[end + 1..];
    }
    rendered.push_str(rest);
    Ok(rendered.replace("@REPO@", &repo.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_and_the_repository_are_filled_in() {
        let source = "image: \"@IMAGE:tools@\"\npath: \"@REPO@/target\"\nother: \"@IMAGE:relay@\"";
        let rendered = render(source, Path::new("/repo"), |name| {
            Ok(format!("localhost/{name}:1"))
        })
        .unwrap();
        assert_eq!(
            rendered,
            "image: \"localhost/tools:1\"\npath: \"/repo/target\"\nother: \"localhost/relay:1\""
        );
    }

    #[test]
    fn an_unclosed_image_is_refused() {
        assert!(render("@IMAGE:tools", Path::new("/"), |_| Ok(String::new())).is_err());
    }

    #[test]
    fn every_pod_has_a_pod_file_naming_known_images() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for pod in Pod::ALL {
            let file = pod.file(repo);
            let source = std::fs::read_to_string(&file).unwrap();
            assert!(
                source.contains(&format!("name: \"{}\"", pod.podman_name())),
                "{file:?}"
            );
            render(&source, repo, |name| {
                images::find(name)
                    .map(|_| String::new())
                    .ok_or_else(|| Error::Failed(name.to_owned()))
            })
            .unwrap();
        }
    }
}
