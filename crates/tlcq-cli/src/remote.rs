//! The laptop's calls to the runner (design D6, D12): a seam, so the commands
//! run against `docker --context pve exec -i tlcq-runner tlcq <verb>` in use and
//! against a runner in the same process in tests.

use crate::commit::CommitReply;
use std::collections::BTreeMap;

use crate::proto::{
    CancelRequest, FetchRequest, PreflightReply, PreflightRequest, ResumeReply, ResumeRequest,
    VmJob,
};

/// Why a VM call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    /// It could not connect: the VM is unreachable.
    #[error("the VM is unreachable: {0}")]
    Unreachable(String),
    /// It connected, then failed: what it did on the VM is unknown.
    #[error("the VM call failed: {0}")]
    Failed(String),
}

/// The VM verbs the laptop calls.
pub trait Remote {
    /// The preflight.
    ///
    /// # Errors
    /// If the call fails.
    fn preflight(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError>;

    /// A poll: the preflight's answer, after the VM has settled and admitted.
    ///
    /// # Errors
    /// If the call fails.
    fn status(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError>;

    /// The commit, as it goes on the wire.
    ///
    /// # Errors
    /// If the call fails.
    fn commit(&mut self, wire: &[u8]) -> Result<CommitReply, CallError>;

    /// Every job the VM holds, by id.
    ///
    /// # Errors
    /// If the call fails.
    fn ls(&mut self) -> Result<BTreeMap<String, VmJob>, CallError>;

    /// A cancel: the job's state after it.
    ///
    /// # Errors
    /// If the call fails.
    fn cancel(&mut self, req: &CancelRequest) -> Result<VmJob, CallError>;

    /// A resume: the job's state after it, and why it was refused.
    ///
    /// # Errors
    /// If the call fails.
    fn resume(&mut self, req: &ResumeRequest) -> Result<ResumeReply, CallError>;

    /// A run's files: the reply line, then each file's bytes.
    ///
    /// # Errors
    /// If the call fails.
    fn fetch(&mut self, req: &FetchRequest) -> Result<Vec<u8>, CallError>;
}
