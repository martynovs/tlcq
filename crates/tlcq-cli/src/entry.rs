//! Who submitted a job: entries, their keys and labels (design D6).
//!
//! A job keeps one entry per submitter. Two jobs whose entries share a key and
//! have different ids are two versions of one configuration in one worktree.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What makes two jobs versions of each other.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Key {
    /// The worktree's path.
    pub worktree: PathBuf,
    /// The model directory, relative to the worktree.
    pub model_dir: PathBuf,
    /// The spec module.
    pub spec: String,
    /// The configuration's file name.
    pub cfg_name: String,
}

/// Who an entry belongs to, for reports.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Label {
    /// The git top-level directory's name.
    pub project: String,
    /// The worktree's path.
    pub worktree: PathBuf,
    /// The worktree's branch, if any.
    pub branch: Option<String>,
    /// `$TLCQ_AGENT`, if set.
    pub agent: Option<String>,
}

impl Label {
    /// Whether this label is the same agent as `other`: the same worktree and
    /// `$TLCQ_AGENT` (unset matching unset), whatever the branch.
    pub fn same_agent(&self, other: &Label) -> bool {
        self.worktree == other.worktree && self.agent == other.agent
    }
}

/// `project/branch` (the worktree's directory name when detached), then
/// `:agent` if set: how reports and cancel markers name a submitter.
impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let place = self.branch.clone().unwrap_or_else(|| {
            self.worktree.file_name().map_or_else(
                || self.worktree.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        });
        write!(f, "{}/{place}", self.project)?;
        if let Some(agent) = &self.agent {
            write!(f, ":{agent}")?;
        }
        Ok(())
    }
}

/// One submitter of a job.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Entry {
    /// The configuration it holds.
    pub key: Key,
    /// Who it is.
    pub label: Label,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(worktree: &str, branch: &str, agent: Option<&str>) -> Label {
        Label {
            project: "specs".to_owned(),
            worktree: PathBuf::from(worktree),
            branch: Some(branch.to_owned()),
            agent: agent.map(str::to_owned),
        }
    }

    #[test]
    fn the_same_agent_is_the_worktree_and_agent_whatever_the_branch() {
        let a = label("/w1", "main", None);
        assert!(a.same_agent(&label("/w1", "feature", None)));
        assert!(!a.same_agent(&label("/w2", "main", None)));
        assert!(!a.same_agent(&label("/w1", "main", Some("x"))));
    }

    #[test]
    fn a_label_reads_project_branch_and_agent() {
        assert_eq!(
            label("/w/wt1", "main", Some("a1")).to_string(),
            "specs/main:a1"
        );
        let detached = Label {
            branch: None,
            ..label("/w/wt1", "x", None)
        };
        assert_eq!(detached.to_string(), "specs/wt1");
        let root = Label {
            branch: None,
            ..label("/", "x", None)
        };
        assert_eq!(root.to_string(), "specs//");
    }
}
