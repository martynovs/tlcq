//! The engine a job runs on (tlx-engine E1, E2): tlx, the default, or TLC; and
//! what identifies each on one machine.
//!
//! tlx is pinned to one git rev. The laptop's install and the image build that
//! rev and write it to a `rev` file beside the binary, since the laptop's and
//! the VM's binaries differ (macOS, Linux) and tlx reports no commit of its own.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A model checker.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// tlx, the Rust rewrite of TLC: the default for a submit.
    #[default]
    Tlx,
    /// TLC, from the jars.
    Tlc,
}

impl Engine {
    /// The engine of a job description, checkpoint or history record stored
    /// without one: everything stored before tlx was TLC's.
    pub fn stored() -> Self {
        Engine::Tlc
    }

    /// Its name, as the config and the reports spell it.
    pub fn name(self) -> &'static str {
        match self {
            Engine::Tlx => "tlx",
            Engine::Tlc => "tlc",
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What identifies each engine on one machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    /// The TLC jars' version (`id::jar_version`).
    pub jar_version: String,
    /// The git rev tlx was built from; `None` when tlx is not installed.
    #[serde(default)]
    pub tlx_rev: Option<String>,
}

impl Versions {
    /// `engine`'s identity, as a job's manifest carries it; `None` for tlx
    /// when it is not installed.
    pub fn identity(&self, engine: Engine) -> Option<String> {
        match engine {
            Engine::Tlc => Some(format!("tlc {}", self.jar_version)),
            Engine::Tlx => self.tlx_rev.as_ref().map(|rev| format!("tlx {rev}")),
        }
    }

    /// Whether `other` identifies `engine` as these do, both having it.
    pub fn matches(&self, other: &Versions, engine: Engine) -> bool {
        self.identity(engine)
            .is_some_and(|mine| other.identity(engine).as_ref() == Some(&mine))
    }
}

/// The file beside the tlx binary that names the rev it was built from.
pub const REV: &str = "rev";

/// The rev of the tlx binary at `tlx`, from the `rev` file beside it; `None`
/// when the binary or the file is missing, or the file is empty.
///
/// # Errors
/// If the file exists but cannot be read.
pub fn tlx_rev(tlx: &Path) -> std::io::Result<Option<String>> {
    if !tlx.is_file() {
        return Ok(None);
    }
    match std::fs::read_to_string(tlx.with_file_name(REV)) {
        Ok(text) => Ok(Some(text.trim().to_owned()).filter(|rev| !rev.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Versions with the jar version `jar` and tlx at rev `R`, for tests.
#[cfg(test)]
pub(crate) fn test_versions(jar: &str) -> Versions {
    Versions {
        jar_version: jar.to_owned(),
        tlx_rev: Some("R".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(rev: Option<&str>) -> Versions {
        Versions {
            jar_version: "J".to_owned(),
            tlx_rev: rev.map(str::to_owned),
        }
    }

    #[test]
    fn identities_name_the_engine_and_its_version() {
        let v = versions(Some("abc"));
        assert_eq!(v.identity(Engine::Tlc).as_deref(), Some("tlc J"));
        assert_eq!(v.identity(Engine::Tlx).as_deref(), Some("tlx abc"));
        assert_eq!(versions(None).identity(Engine::Tlx), None);
        assert_eq!(Engine::Tlx.to_string(), "tlx");
        assert_eq!(Engine::stored(), Engine::Tlc);
    }

    #[test]
    fn versions_match_per_engine() {
        let laptop = versions(Some("abc"));
        let stale = versions(Some("old"));
        assert!(laptop.matches(&laptop, Engine::Tlx));
        assert!(!laptop.matches(&stale, Engine::Tlx));
        assert!(laptop.matches(&stale, Engine::Tlc));
        assert!(!versions(None).matches(&versions(None), Engine::Tlx));
    }

    #[test]
    fn the_rev_is_read_beside_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let tlx = dir.path().join("tlx");
        assert_eq!(tlx_rev(&tlx).unwrap(), None);
        std::fs::write(&tlx, "").unwrap();
        assert_eq!(tlx_rev(&tlx).unwrap(), None);
        std::fs::write(dir.path().join(REV), "\n").unwrap();
        assert_eq!(tlx_rev(&tlx).unwrap(), None);
        std::fs::write(dir.path().join(REV), "bec5985\n").unwrap();
        assert_eq!(tlx_rev(&tlx).unwrap().as_deref(), Some("bec5985"));
        std::fs::remove_file(dir.path().join(REV)).unwrap();
        std::fs::create_dir(dir.path().join(REV)).unwrap();
        tlx_rev(&tlx).unwrap_err();
    }
}
