//! A job's content-hash id (design D3).
//!
//! The id is the SHA-256 of a manifest: the spec module name; the tarball's
//! paths in order, each with the SHA-256 of its bytes; the options; and the
//! engine's identity (`engine::Versions::identity`: TLC with its jar version,
//! or tlx with its rev). The configuration's name is not in it (the configuration travels as
//! `model/job.cfg`), so renaming a configuration keeps its results.

use std::fmt::Write as _;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::job::Contents;

/// The jars whose hashes make the jar version, in order.
pub const JARS: [&str; 2] = ["tla2tools.jar", "CommunityModules-deps.jar"];

/// The jar version of the jars in `dir`: the SHA-256 over each jar's name and hash.
/// Each machine computes its own, and the VM refuses a job whose version differs.
pub fn jar_version(dir: &Path) -> std::io::Result<String> {
    let lines = JARS
        .iter()
        .map(|jar| {
            Ok(format!(
                "{jar} {}\n",
                sha256(&std::fs::read(dir.join(jar))?)
            ))
        })
        .collect::<std::io::Result<String>>()?;
    Ok(sha256(lines.as_bytes()))
}

/// The manifest a job's id hashes, its engine's identity last. One line per
/// item, so no two jobs share one.
pub fn manifest(spec: &str, contents: &Contents, options: &[String], identity: &str) -> String {
    let mut m = format!("spec {spec}\n");
    for f in contents.files() {
        writeln!(m, "file {} {}", f.path, sha256(&f.bytes)).expect(TO_STRING);
    }
    for o in options {
        writeln!(m, "option {o}").expect(TO_STRING);
    }
    writeln!(m, "engine {identity}").expect(TO_STRING);
    m
}

/// The id of a manifest: its SHA-256, in hex.
pub fn id(manifest: &str) -> String {
    sha256(manifest.as_bytes())
}

/// The SHA-256 of `bytes`, in lower-case hex.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            write!(s, "{b:02x}").expect(TO_STRING);
            s
        })
}

/// Writing to a `String` cannot fail.
const TO_STRING: &str = "writing to a String cannot fail";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{File, Limits};

    fn contents(files: &[(&str, &str)]) -> Contents {
        let files = files
            .iter()
            .map(|(p, b)| File {
                path: (*p).to_owned(),
                bytes: b.as_bytes().to_vec(),
            })
            .collect();
        Contents::new(files, Limits::default()).unwrap()
    }

    #[test]
    fn sha256_of_a_known_string() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_manifest_lists_spec_files_options_and_engine() {
        let c = contents(&[("model/job.cfg", "cfg"), ("model/A.tla", "a")]);
        let m = manifest("A", &c, &["-deadlock".to_owned()], "tlc J");
        assert_eq!(
            m,
            format!(
                "spec A\nfile model/A.tla {}\nfile model/job.cfg {}\noption -deadlock\nengine tlc J\n",
                sha256(b"a"),
                sha256(b"cfg")
            )
        );
    }

    #[test]
    fn the_id_follows_what_is_checked() {
        let base = contents(&[("model/job.cfg", "cfg"), ("model/A.tla", "a")]);
        let id_of = |spec, c: &Contents, opts: &[String], jar| id(&manifest(spec, c, opts, jar));
        let one = id_of("A", &base, &[], "J");
        assert_eq!(one.len(), 64);
        assert_eq!(one, id_of("A", &base, &[], "J"));
        assert_ne!(one, id_of("B", &base, &[], "J"));
        assert_ne!(one, id_of("A", &base, &["-deadlock".to_owned()], "J"));
        assert_ne!(one, id_of("A", &base, &[], "K"));
        assert_ne!(
            id_of("A", &base, &[], "tlc J"),
            id_of("A", &base, &[], "tlx J")
        );
        let edited = contents(&[("model/job.cfg", "cfg2"), ("model/A.tla", "a")]);
        assert_ne!(one, id_of("A", &edited, &[], "J"));
    }

    #[test]
    fn the_jar_version_hashes_both_jars() {
        let dir = tempfile::tempdir().unwrap();
        for jar in JARS {
            std::fs::write(dir.path().join(jar), jar).unwrap();
        }
        let v = jar_version(dir.path()).unwrap();
        assert_eq!(v.len(), 64);
        std::fs::write(dir.path().join(JARS[1]), "changed").unwrap();
        assert_ne!(v, jar_version(dir.path()).unwrap());
    }

    #[test]
    fn a_missing_jar_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        jar_version(dir.path()).unwrap_err();
    }
}
