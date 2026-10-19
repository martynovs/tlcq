//! A job's contents and its tarball (design D2).
//!
//! A job is every `*.tla` of the model directory and of the directories its
//! spec EXTENDS (D4), except TLC's trace files, plus the one configuration it
//! checks. The tarball lays them out as `model/` (the configuration as
//! `model/job.cfg`) and `lib/0/`, `lib/1/`…, one per EXTENDS directory. The
//! VM's tarball checks (names, file types, size and entry limits) run here too,
//! at input, so a job the VM would refuse is refused before anything is written
//! (D6).

use std::path::{Path, PathBuf};

/// The configuration's path inside the tarball.
pub const CFG: &str = "model/job.cfg";

/// Size limits on a job (D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Total uncompressed bytes.
    pub bytes: u64,
    /// Number of files.
    pub entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 16 * 1024 * 1024,
            entries: 2000,
        }
    }
}

/// One file of a job: its path inside the tarball and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The path inside the tarball, `/`-separated.
    pub path: String,
    /// The file's bytes.
    pub bytes: Vec<u8>,
}

/// A checked job: its files, sorted by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contents {
    files: Vec<File>,
}

/// Why a job could not be built.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A directory or file could not be read.
    #[error("{path}: {source}")]
    Read {
        /// What was being read.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A module is not a regular file (a link or a directory).
    #[error("{path}: not a regular file")]
    NotAFile {
        /// The entry.
        path: PathBuf,
    },
    /// A path the VM would refuse.
    #[error("{path:?}: names may hold only letters, digits, `_`, `.` and `-`, with no `..`")]
    BadName {
        /// The path inside the tarball, or the file name.
        path: String,
    },
    /// Two files at one path.
    #[error("{path}: twice in the job")]
    Duplicate {
        /// The path inside the tarball.
        path: String,
    },
    /// More files than the limit.
    #[error("{count} files, over the limit of {limit}")]
    TooMany {
        /// Files in the job.
        count: usize,
        /// The limit.
        limit: usize,
    },
    /// More bytes than the limit.
    #[error("{bytes} bytes, over the limit of {limit}")]
    TooBig {
        /// Bytes in the job.
        bytes: u64,
        /// The limit.
        limit: u64,
    },
}

impl Contents {
    /// Check `files` as the VM does and sort them by path.
    pub fn new(mut files: Vec<File>, limits: Limits) -> Result<Self, Error> {
        if files.len() > limits.entries {
            return Err(Error::TooMany {
                count: files.len(),
                limit: limits.entries,
            });
        }
        let bytes = files.iter().map(|f| f.bytes.len() as u64).sum();
        if bytes > limits.bytes {
            return Err(Error::TooBig {
                bytes,
                limit: limits.bytes,
            });
        }
        if let Some(f) = files.iter().find(|f| !is_safe_path(&f.path)) {
            return Err(Error::BadName {
                path: f.path.clone(),
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some([a, _]) = files
            .windows(2)
            .find(|w| matches!(w, [a, b] if a.path == b.path))
        {
            return Err(Error::Duplicate {
                path: a.path.clone(),
            });
        }
        Ok(Self { files })
    }

    /// The files, sorted by path.
    pub fn files(&self) -> &[File] {
        &self.files
    }

    /// The job's tarball: the files in path order, with fixed modes, owners and
    /// times, so its bytes depend only on the files.
    pub fn tarball(&self) -> std::io::Result<Vec<u8>> {
        let mut builder = tar::Builder::new(Vec::new());
        for f in &self.files {
            let mut header = tar::Header::new_ustar();
            header.set_path(&f.path)?;
            header.set_size(f.bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, f.bytes.as_slice())?;
        }
        builder.into_inner()
    }
}

/// Collect the job of `dir`, with the modules of each `extends` directory, checking the configuration `cfg`.
pub fn collect(
    dir: &Path,
    extends: &[PathBuf],
    cfg: &[u8],
    limits: Limits,
) -> Result<Contents, Error> {
    with_cfg(&modules_of(dir, extends)?, cfg, limits)
}

/// The modules of `dir` under `model/`, and of each `extends` directory under
/// `lib/<n>/`: what every job of the directory shares.
pub fn modules_of(dir: &Path, extends: &[PathBuf]) -> Result<Vec<File>, Error> {
    let mut files = modules(dir, "model")?;
    for (n, lib) in extends.iter().enumerate() {
        files.extend(modules(&dir.join(lib), &format!("lib/{n}"))?);
    }
    Ok(files)
}

/// A job: `modules` (from [`modules_of`]) and the configuration `cfg`, checked.
pub fn with_cfg(modules: &[File], cfg: &[u8], limits: Limits) -> Result<Contents, Error> {
    let mut files = modules.to_vec();
    files.push(File {
        path: CFG.to_owned(),
        bytes: cfg.to_vec(),
    });
    Contents::new(files, limits)
}

/// Every module of `dir` (`*.tla`, except TLC's `*_TTrace_*.tla`), under `prefix`.
fn modules(dir: &Path, prefix: &str) -> Result<Vec<File>, Error> {
    let read = |path: &Path, source| Error::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| read(dir, e))? {
        let entry = entry.map_err(|e| read(dir, e))?;
        // A name that is not UTF-8 comes out with U+FFFD, which `Contents::new` refuses.
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_module = Path::new(&name).extension().is_some_and(|e| e == "tla");
        // Hidden names (an editor's `.#Spec.tla` lock link) are no module of TLC's.
        if !is_module || name.starts_with('.') || name.contains("_TTrace_") {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| read(&path, e))?;
        if !meta.is_file() {
            return Err(Error::NotAFile { path });
        }
        let bytes = std::fs::read(&path).map_err(|e| read(&path, e))?;
        files.push(File {
            path: format!("{prefix}/{name}"),
            bytes,
        });
    }
    Ok(files)
}

/// Whether the VM accepts `path`: relative, `/`-separated, each part made of
/// letters, digits, `_`, `.` and `-`, and none of them `.` or `..`.
pub fn is_safe_path(path: &str) -> bool {
    path.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: &str) -> File {
        File {
            path: path.to_owned(),
            bytes: bytes.as_bytes().to_vec(),
        }
    }

    fn extends(dirs: &[&str]) -> Vec<PathBuf> {
        dirs.iter().map(PathBuf::from).collect()
    }

    fn paths(c: &Contents) -> Vec<&str> {
        c.files().iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn collects_modules_extends_and_the_configuration() {
        let root = tempfile::tempdir().unwrap();
        let model = root.path().join("juliet");
        let lib = root.path().join("november");
        std::fs::create_dir(&model).unwrap();
        std::fs::create_dir(&lib).unwrap();
        std::fs::write(model.join("Juliet.tla"), "juliet").unwrap();
        std::fs::write(model.join("Juliet_TTrace_1.tla"), "trace").unwrap();
        std::os::unix::fs::symlink("user@host.1234", model.join(".#Juliet.tla")).unwrap();
        std::fs::write(model.join("JA.cfg"), "other cfg").unwrap();
        std::fs::create_dir(model.join("states")).unwrap();
        std::fs::write(lib.join("November.tla"), "november").unwrap();
        let c = collect(
            &model,
            &extends(&["../november"]),
            b"cfg",
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            paths(&c),
            ["lib/0/November.tla", "model/Juliet.tla", "model/job.cfg"]
        );
        assert_eq!(c.files().last().unwrap().bytes, b"cfg");
    }

    #[test]
    fn a_name_the_vm_refuses_is_refused_at_input() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Spec copy.tla"), "").unwrap();
        let err = collect(dir.path(), &extends(&[]), b"", Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::BadName { ref path } if path == "model/Spec copy.tla"),
            "{err}"
        );
    }

    #[test]
    fn a_link_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Real.tla"), "").unwrap();
        std::os::unix::fs::symlink(dir.path().join("Real.tla"), dir.path().join("Link.tla"))
            .unwrap();
        let err = collect(dir.path(), &extends(&[]), b"", Limits::default()).unwrap_err();
        assert!(matches!(err, Error::NotAFile { .. }), "{err}");
    }

    #[test]
    fn a_missing_directory_is_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = collect(
            &dir.path().join("nope"),
            &extends(&[]),
            b"",
            Limits::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Read { .. }), "{err}");
    }

    #[test]
    fn limits_are_enforced() {
        let limits = Limits {
            bytes: 3,
            entries: 1,
        };
        let two = vec![file("model/a.tla", ""), file("model/b.tla", "")];
        assert!(matches!(
            Contents::new(two, limits),
            Err(Error::TooMany { count: 2, limit: 1 })
        ));
        let big = vec![file("model/a.tla", "1234")];
        assert!(matches!(
            Contents::new(big, limits),
            Err(Error::TooBig { bytes: 4, limit: 3 })
        ));
    }

    #[test]
    fn duplicates_are_refused() {
        let dup = vec![file("model/a.tla", ""), file("model/a.tla", "")];
        assert!(matches!(
            Contents::new(dup, Limits::default()),
            Err(Error::Duplicate { .. })
        ));
    }

    #[test]
    fn safe_paths() {
        assert!(is_safe_path("lib/0/November.tla"));
        for bad in [
            "/abs.tla",
            "model/../x.tla",
            "model/./x.tla",
            "a//b",
            "model/a b.tla",
            "model/ä.tla",
            "",
        ] {
            assert!(!is_safe_path(bad), "{bad}");
        }
    }

    #[test]
    fn the_tarball_is_deterministic_and_holds_the_files() {
        let files = vec![file("model/job.cfg", "cfg"), file("model/A.tla", "a")];
        let c = Contents::new(files, Limits::default()).unwrap();
        let tar = c.tarball().unwrap();
        assert_eq!(tar, c.tarball().unwrap());
        let mut archive = tar::Archive::new(tar.as_slice());
        let read: Vec<(String, Vec<u8>, u64)> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let path = e.path().unwrap().to_string_lossy().into_owned();
                let mtime = e.header().mtime().unwrap();
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut e, &mut bytes).unwrap();
                (path, bytes, mtime)
            })
            .collect();
        assert_eq!(
            read,
            [
                ("model/A.tla".to_owned(), b"a".to_vec(), 0),
                ("model/job.cfg".to_owned(), b"cfg".to_vec(), 0)
            ]
        );
    }
}
