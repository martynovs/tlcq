//! The commit (design D6): what the laptop sends to the runner in one call, and
//! what it answers.
//!
//! On the wire: one line of JSON, the [`CommitRequest`], then each job's tarball,
//! in the order of `jobs`, `tar_len` bytes each. Only fixed verbs and ids travel
//! on the command line.

use std::collections::BTreeMap;
use std::io::{BufRead, Read};

use serde::{Deserialize, Serialize};

use crate::entry::{Entry, Key};
use crate::job::{self, Contents, Limits};
use crate::outcome::JobResult;
use crate::proto::{JobInfo, RunRef};

/// How a new job asks to be placed on the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitRoute {
    /// The queue (long, max and unknown jobs).
    Queue,
    /// Start now, or refuse: the laptop holds a slot for it (a short job).
    NowOrRefuse,
    /// Start now, or queue.
    NowOrQueue,
}

/// One job of a commit; its tarball follows the request on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitJob {
    /// The id the laptop computed; the VM recomputes it.
    pub id: String,
    /// Its description, `job.json`.
    pub info: JobInfo,
    /// How to place it, if it is new on the VM.
    pub route: CommitRoute,
    /// Its tarball's length.
    pub tar_len: u64,
}

/// An entry attached while the VM was unreachable, flushed onto its own run only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFlush {
    /// The job's id.
    pub id: String,
    /// The run it attached to.
    pub run: RunRef,
    /// The entry.
    pub entry: Entry,
}

/// An older version to supersede on the VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersedeOrder {
    /// The older version's id.
    pub old: String,
    /// The key whose entry goes.
    pub key: Key,
    /// The new id, for "replaced by".
    pub by: String,
    /// The canceller's label, for the report.
    pub label: String,
    /// Whether this commit also attaches to it: then it is never cancelled.
    pub attached: bool,
}

/// The commit's request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRequest {
    /// The laptop's engine versions.
    #[serde(flatten)]
    pub versions: crate::engine::Versions,
    /// The commit's token (D6, Tokens).
    pub token: String,
    /// Pending entries, applied first.
    pub pending: Vec<PendingFlush>,
    /// The jobs, attaches included, applied next.
    pub jobs: Vec<CommitJob>,
    /// The supersedes, applied after the jobs.
    pub supersede: Vec<SupersedeOrder>,
}

/// What the VM did with one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum JobAnswer {
    /// Queued as a new run, and started by the admission round.
    Started {
        /// The run.
        run: String,
    },
    /// Queued as a new run.
    Queued {
        /// The run.
        run: String,
    },
    /// Attached to its live run.
    Attached {
        /// The run.
        run: String,
    },
    /// The VM's cache holds its result.
    Cached {
        /// The result.
        result: JobResult,
    },
    /// A `now-or-refuse` job the VM could not start: not stored.
    Refused,
}

/// The commit's reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "commit", rename_all = "snake_case")]
pub enum CommitReply {
    /// A job failed the VM's checks: nothing was applied.
    Invalid {
        /// The job.
        id: String,
        /// Why.
        reason: String,
    },
    /// The token was tombstoned (a lookup found it before this commit arrived):
    /// nothing was applied.
    NotApplied,
    /// Applied.
    Applied {
        /// Each job's answer, by id.
        jobs: BTreeMap<String, JobAnswer>,
        /// Pending entries dropped because their run had ended or was unknown.
        dropped: Vec<PendingFlush>,
        /// Each job as it stands after the commit's admission round: its
        /// slot, or its place in the queue, for the report.
        #[serde(default)]
        views: BTreeMap<String, crate::proto::VmJob>,
    },
}

/// Why a commit could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The stream ended or failed.
    #[error("reading the commit: {0}")]
    Io(#[from] std::io::Error),
    /// The request line is not a request.
    #[error("the commit's request: {0}")]
    Json(#[from] serde_json::Error),
    /// A tarball longer than the limit: refused before reading it.
    #[error("{id}: a tarball of {len} bytes, over the limit")]
    TooBig {
        /// The job.
        id: String,
        /// Its tarball's length.
        len: u64,
    },
}

/// The commit as it goes on the wire: the request line, then the tarballs.
pub fn encode(req: &CommitRequest, tarballs: &[Vec<u8>]) -> serde_json::Result<Vec<u8>> {
    let mut out = serde_json::to_vec(req)?;
    out.push(b'\n');
    for t in tarballs {
        out.extend_from_slice(t);
    }
    Ok(out)
}

/// Read a commit from the wire: the request and each job's tarball. A tarball
/// longer than `limit` bytes is refused before it is read.
pub fn decode(
    mut input: impl BufRead,
    limit: u64,
) -> Result<(CommitRequest, Vec<Vec<u8>>), ReadError> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let req: CommitRequest = serde_json::from_str(&line)?;
    if let Some(job) = req.jobs.iter().find(|j| j.tar_len > limit) {
        return Err(ReadError::TooBig {
            id: job.id.clone(),
            len: job.tar_len,
        });
    }
    let mut tarballs = Vec::new();
    for job in &req.jobs {
        let mut bytes = Vec::new();
        (&mut input).take(job.tar_len).read_to_end(&mut bytes)?;
        if (bytes.len() as u64) < job.tar_len {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        tarballs.push(bytes);
    }
    // Read to the end, so the payload's end is seen (the VM's deadline watches
    // for it); anything after the tarballs is no commit's.
    let mut rest = [0; 1];
    if input.read(&mut rest)? > 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bytes after the last tarball",
        )
        .into());
    }
    Ok((req, tarballs))
}

/// Check a commit's job as the VM does (D2, D3): its tarball holds only regular
/// files (and directories) under safe names, within the limits, and its id is
/// the hash of what it holds with the VM's own identity of the job's engine.
pub fn check_job(
    job: &CommitJob,
    tarball: &[u8],
    versions: &crate::engine::Versions,
    limits: Limits,
) -> Result<Contents, String> {
    // Names that become paths and container names on the VM: a moved run's
    // laptop name (`l<n>`) and the spec module.
    if let Some(run) = &job.info.run {
        let n = run.strip_prefix('l').unwrap_or_default();
        if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{run}: not a laptop run's name"));
        }
    }
    if !crate::config::is_module_name(&job.info.spec) {
        return Err(format!("{}: not a module name", job.info.spec));
    }
    let contents = read_tarball(tarball, limits)?;
    let engine = job.info.engine;
    let identity = versions
        .identity(engine)
        .ok_or_else(|| format!("{engine} is not installed on the VM"))?;
    let manifest = crate::id::manifest(&job.info.spec, &contents, &job.info.tlc_opts, &identity);
    let id = crate::id::id(&manifest);
    if id != job.id {
        return Err(format!(
            "its id is {id}, not {}: the {engine} versions or the files differ",
            job.id
        ));
    }
    Ok(contents)
}

/// A tarball's files, checked: regular files only (directories skipped), every
/// path safe, within the limits.
pub fn read_tarball(tarball: &[u8], limits: Limits) -> Result<Contents, String> {
    let mut files = Vec::new();
    let mut archive = tar::Archive::new(tarball);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.header().entry_type();
        let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(format!("{path}: not a regular file"));
        }
        if files.len() >= limits.entries {
            return Err(format!("over {} files", limits.entries));
        }
        let mut bytes = Vec::new();
        (&mut entry)
            .take(limits.bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        files.push(job::File { path, bytes });
    }
    Contents::new(files, limits).map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "commit_tests.rs"]
pub(crate) mod tests;
