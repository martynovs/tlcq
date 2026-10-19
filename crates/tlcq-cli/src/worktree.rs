//! Where a submission comes from (design D6, D13): the git worktree holding the
//! model directory, the directory within it, and the submitter's label.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::entry::Label;

/// A model directory placed in its worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The worktree's top-level path.
    pub worktree: PathBuf,
    /// The model directory, relative to the worktree.
    pub model_dir: PathBuf,
    /// The submitter.
    pub label: Label,
}

/// The origin of the model directory `dir`, with `$TLCQ_AGENT` as `agent`.
///
/// # Errors
/// If `dir` is not in a git worktree.
pub fn origin(dir: &Path, agent: Option<String>) -> std::io::Result<Origin> {
    let dir = dir.canonicalize()?;
    let worktree = PathBuf::from(git(&dir, &["rev-parse", "--show-toplevel"])?.ok_or_else(
        || std::io::Error::other(format!("{}: not in a git worktree", dir.display())),
    )?)
    .canonicalize()?;
    let model_dir = dir
        .strip_prefix(&worktree)
        .map_or_else(|_| dir.clone(), Path::to_path_buf);
    let project = worktree
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let branch = git(&dir, &["branch", "--show-current"])?.filter(|b| !b.is_empty());
    Ok(Origin {
        label: Label {
            project,
            worktree: worktree.clone(),
            branch,
            agent,
        },
        worktree,
        model_dir,
    })
}

/// A git command's trimmed output in `dir`; `None` if git refuses.
fn git(dir: &Path, args: &[&str]) -> std::io::Result<Option<String>> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()))
}

/// A random token for a commit (D6): unique enough that no two commits share one.
pub fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.write_u32(std::process::id());
    let a = h.finish();
    h.write_u64(a);
    format!("{a:016x}{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_model_directory_is_placed_in_its_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("specs");
        std::fs::create_dir_all(repo.join("tla/delta")).unwrap();
        run(&repo, &["init", "--quiet", "--initial-branch", "main"]);
        let o = origin(&repo.join("tla/delta"), Some("a1".to_owned())).unwrap();
        assert_eq!(o.worktree, repo.canonicalize().unwrap());
        assert_eq!(o.model_dir, PathBuf::from("tla/delta"));
        assert_eq!(o.label.to_string(), "specs/main:a1");
        run(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        run(&repo, &["checkout", "--quiet", "--detach"]);
        let detached = origin(&repo, None).unwrap();
        assert_eq!(
            (detached.label.branch.clone(), detached.label.to_string()),
            (None, "specs/specs".to_owned())
        );
        let outside = tempfile::tempdir().unwrap();
        origin(outside.path(), None).unwrap_err();
    }

    #[test]
    fn tokens_differ() {
        assert_ne!(token(), token());
        assert_eq!(token().len(), 32);
    }
}
