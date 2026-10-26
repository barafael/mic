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
use crate::settings::Settings;

/// The box's network: `--internal`, so nothing in it reaches the host's network or beyond.
pub const NETWORK: &str = "mic-testbox";
/// What every pod's name starts with; kube play names each container `<pod>-<container>`.
pub const PREFIX: &str = "mic-testbox-";
/// What each phase leaves for looking into it (`checks::Record`).
pub const RECORD: &str = "target/testbox-record";
/// The pod files with their images and paths filled in, which `kube down` needs again.
const RENDERED: &str = "target/testbox-pods";
/// A directory per client for its display's socket, which only that client mounts: no client
/// reaches another's display.
pub const VIEW: &str = "target/testbox-view";
/// The clients `up` plays, each a pod of its own from testbox/pods/client.yaml.
pub const CLIENTS: [&str; 1] = ["alice"];

/// A pod of the box, from `testbox/pods/<name>.yaml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pod {
    /// moq-relay, with the box's TLS.
    Relay,
    /// The checks' tools inside the network.
    Tools,
    /// A participant, by name, with a display of its own.
    Client(&'static str),
    /// Builds what the box runs (`crate::builder`). Not part of the box: it fetches crates, so
    /// it's on podman's default network, and it outlives `down`.
    Builder,
}

impl Pod {
    /// The box's pods, in the order they're played: the relay makes the keys the others read.
    pub fn all() -> Vec<Pod> {
        [Pod::Relay, Pod::Tools]
            .into_iter()
            .chain(CLIENTS.map(Pod::Client))
            .collect()
    }

    pub fn name(self) -> &'static str {
        match self {
            Pod::Relay => "relay",
            Pod::Tools => "tools",
            Pod::Client(name) => name,
            Pod::Builder => "builder",
        }
    }

    /// Its pod file's name under testbox/pods: every client is played from one.
    fn source(self) -> &'static str {
        match self {
            Pod::Client(_) => "client",
            other => other.name(),
        }
    }

    /// The network it's played on.
    fn network(self) -> &'static str {
        match self {
            Pod::Relay | Pod::Tools | Pod::Client(_) => NETWORK,
            Pod::Builder => "podman",
        }
    }

    /// The pod file as it was last played.
    fn rendered(self, repo: &Path) -> PathBuf {
        repo.join(RENDERED).join(format!("{}.yaml", self.name()))
    }

    pub fn file(self, repo: &Path) -> PathBuf {
        repo.join("testbox/pods")
            .join(format!("{}.yaml", self.source()))
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
    /// How it was brought up, or is to be.
    settings: Settings,
}

/// The marker of the pod files' lines that pass the host's GPU through.
const GPU: &str = "# @GPU@ ";
/// The host's render node.
pub const RENDER_NODE: &str = "/dev/dri/renderD128";

impl TestBox {
    /// The box as `up` last brought it up here, or with the defaults.
    pub fn new(repo: &Path) -> Result<Self> {
        Ok(Self::with(repo, Settings::saved(repo)?))
    }

    /// The box with these settings, which `up` keeps for what plays it again later.
    pub fn with(repo: &Path, settings: Settings) -> Self {
        Self {
            repo: repo.to_owned(),
            settings,
        }
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// Builds the images that are missing and what the box runs, makes the network, and plays
    /// every pod of the box.
    pub fn up(&self) -> Result<()> {
        if self.settings.gpu && !Path::new(RENDER_NODE).exists() {
            return Err(Error::Failed(format!(
                "no {RENDER_NODE} on this host: `cargo testbox up --no-gpu`"
            )));
        }
        self.settings.save(&self.repo)?;
        self.images()?;
        crate::builder::build(self)?;
        self.network()?;
        self.view_directories()?;
        for pod in Pod::all() {
            self.play(pod)?;
        }
        crate::ready::all(self)
    }

    /// Each client's directory for its display's socket: the owner's only, whatever is in it,
    /// so a link a container plants there leads nowhere.
    fn view_directories(&self) -> Result<()> {
        for client in CLIENTS {
            let directory = self.view_socket(client).with_file_name("");
            error::create_dir_all(&directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for each in [self.repo.join(VIEW), directory.clone()] {
                    std::fs::set_permissions(&each, std::fs::Permissions::from_mode(0o700))
                        .map_err(|source| Error::Io {
                            what: each.display().to_string(),
                            source,
                        })?;
                }
            }
        }
        Ok(())
    }

    /// Where `client`'s display serves on the host (with-display's socket, /run/view/vnc).
    pub fn view_socket(&self, client: &str) -> PathBuf {
        self.repo.join(VIEW).join(client).join("vnc")
    }

    /// Plays `pod`, replacing what runs under its name.
    pub fn play(&self, pod: Pod) -> Result<()> {
        let file = pod.rendered(&self.repo);
        error::create_dir_all(&self.repo.join(RENDERED))?;
        error::write(&file, self.render(pod)?)?;
        let mut play = podman(&["kube", "play", "--replace", "--network", pod.network()]);
        play.arg(&file);
        process::run(play)
    }

    /// The image `container` runs, if it exists.
    pub fn image_of(&self, container: &str) -> Option<String> {
        process::output(podman(&[
            "container",
            "inspect",
            "--format",
            "{{.ImageName}}",
            container,
        ]))
        .ok()
        .map(|name| name.trim().to_owned())
    }

    /// Whether `container` runs; one that isn't there doesn't.
    pub fn running(&self, container: &str) -> bool {
        process::output(podman(&[
            "container",
            "inspect",
            "--format",
            "{{.State.Running}}",
            container,
        ]))
        .is_ok_and(|running| running.trim() == "true")
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

    /// `testbox/pods/<pod>.yaml` with `@REPO@`, `@NAME@` (the pod's name) and each
    /// `@IMAGE:<name>@` filled in.
    fn render(&self, pod: Pod) -> Result<String> {
        let source = pod.file(&self.repo);
        let rendered = render(&error::read_to_string(&source)?, &self.repo, |name| {
            images::find(name)
                .ok_or_else(|| Error::Failed(format!("{}: no image {name}", source.display())))?
                .tag(&self.repo)
        })?;
        Ok(gpu_lines(&rendered, self.settings.gpu).replace("@NAME@", pod.name()))
    }

    /// Stops and removes the box's pods; `wipe` also removes the builder, and then every volume
    /// of the box's (the keys, the builder's caches). Pods share volumes, so the volumes go only
    /// once every pod is down. A pod never played from here is left alone: `kube down` needs the
    /// file it was played from.
    pub fn down(&self, wipe: bool) -> Result<()> {
        let mut pods = Pod::all();
        if wipe {
            pods.push(Pod::Builder);
        }
        // The reverse of the order they're played in: the relay, whose keys the others read,
        // goes last.
        for pod in pods.into_iter().rev() {
            let file = pod.rendered(&self.repo);
            if !file.exists() {
                continue;
            }
            let mut down = podman(&["kube", "down"]);
            down.arg(&file);
            process::run(down)?;
        }
        if wipe {
            let volumes = process::output(podman(&[
                "volume",
                "ls",
                "--quiet",
                "--filter",
                &format!("name=^{PREFIX}"),
            ]))?;
            for volume in volumes.lines() {
                process::run(podman(&["volume", "rm", volume]))?;
            }
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

    /// The containers of `pod`, stopped ones too.
    pub fn containers_of(&self, pod: Pod) -> Vec<String> {
        let prefix = format!("{}-", pod.podman_name());
        self.containers()
            .into_iter()
            .filter(|container| container.starts_with(&prefix))
            .collect()
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

/// `source` with the lines that pass the GPU through kept, without their marker, or left out.
fn gpu_lines(source: &str, gpu: bool) -> String {
    source
        .lines()
        .filter_map(|line| match line.split_once(GPU) {
            Some((indent, rest)) if line.trim_start().starts_with(GPU) => {
                gpu.then(|| format!("{indent}{rest}"))
            }
            _ => Some(line.to_owned()),
        })
        .map(|line| line + "\n")
        .collect()
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
    fn the_gpu_lines_are_kept_or_left_out() {
        let source = "a\n  # @GPU@ { gpu },\nb\n";
        assert_eq!(gpu_lines(source, true), "a\n  { gpu },\nb\n");
        assert_eq!(gpu_lines(source, false), "a\nb\n");
    }

    #[test]
    fn an_unclosed_image_is_refused() {
        assert!(render("@IMAGE:tools", Path::new("/"), |_| Ok(String::new())).is_err());
    }

    #[test]
    fn every_pod_has_a_pod_file_naming_known_images() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for pod in [Pod::Relay, Pod::Tools, Pod::Client("alice"), Pod::Builder] {
            let file = pod.file(repo);
            let source = std::fs::read_to_string(&file)
                .unwrap()
                .replace("@NAME@", pod.name());
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
