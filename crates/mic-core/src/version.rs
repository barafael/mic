//! "Am I on the latest version?" (D15, design §5.12): `version.json` from the site, checked at
//! start, and said in a notice. Nothing updates itself.
//!
//! The major is the wire format: a client of another major can't share a room. Before 1.0, each
//! minor is one, so `min_compatible` is where this client's compatible range starts, as the site
//! writes it (site/build.sh).

use serde::Deserialize;

/// Where the version is read from, unless configured otherwise. No domain yet (docs/observations.md):
/// until there is, the check runs only where a URL is given.
pub const DEFAULT_URL: Option<&str> = None;

/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// What `version.json` says.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Published {
    pub latest: String,
    pub min_compatible: String,
    /// Where to get it, as a URL or a path on the site.
    pub download: String,
}

/// How this build stands against what's published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The latest, or newer (a build from source).
    Latest,
    /// A newer version is out, and this one still shares rooms with it.
    Newer { latest: String, download: String },
    /// A newer version is out that this one can't share rooms with.
    Incompatible { latest: String, download: String },
}

/// How `current` stands against `published`.
pub fn standing(current: &str, published: &Published) -> Result<Standing, String> {
    let parse = |text: &str| {
        semver::Version::parse(text).map_err(|error| format!("version {text:?}: {error}"))
    };
    let (current, latest, oldest) = (
        parse(current)?,
        parse(&published.latest)?,
        parse(&published.min_compatible)?,
    );
    let (latest_text, download) = (published.latest.clone(), published.download.clone());
    Ok(if current >= latest {
        Standing::Latest
    } else if current < oldest {
        Standing::Incompatible {
            latest: latest_text,
            download,
        }
    } else {
        Standing::Newer {
            latest: latest_text,
            download,
        }
    })
}

/// Reads `url` (`version.json`) and says how this build stands, within 10 s. Where to download
/// it is said as a URL, a path on the site taken against `url`.
pub async fn check(url: &str) -> Result<Standing, String> {
    let base = url::Url::parse(url).map_err(|error| format!("{url}: {error}"))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| error.to_string())?;
    let mut published: Published = client
        .get(base.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("{url}: {error}"))?
        .json()
        .await
        .map_err(|error| format!("{url}: {error}"))?;
    if let Ok(download) = base.join(&published.download) {
        published.download = download.into();
    }
    standing(CURRENT, &published)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(latest: &str, min_compatible: &str) -> Published {
        Published {
            latest: latest.to_owned(),
            min_compatible: min_compatible.to_owned(),
            download: "/#download".to_owned(),
        }
    }

    #[test]
    fn the_latest_is_the_latest() {
        assert_eq!(
            standing("0.1.0", &published("0.1.0", "0.1.0")),
            Ok(Standing::Latest)
        );
        assert_eq!(
            standing("0.2.0", &published("0.1.0", "0.1.0")),
            Ok(Standing::Latest)
        );
    }

    #[test]
    fn a_newer_one_is_said_and_whether_rooms_are_shared() {
        assert!(matches!(
            standing("0.1.0", &published("0.1.3", "0.1.0")),
            Ok(Standing::Newer { latest, .. }) if latest == "0.1.3"
        ));
        assert!(matches!(
            standing("0.1.0", &published("0.2.0", "0.2.0")),
            Ok(Standing::Incompatible { .. })
        ));
    }

    #[test]
    fn what_isnt_a_version_is_an_error() {
        assert!(standing("0.1.0", &published("latest", "0.1.0")).is_err());
    }

    #[test]
    fn the_sites_file_reads() {
        let file = r#"{ "latest": "0.1.0", "min_compatible": "0.1.0", "download": "/#download" }"#;
        assert_eq!(
            serde_json::from_str::<Published>(file).unwrap(),
            published("0.1.0", "0.1.0")
        );
    }
}
