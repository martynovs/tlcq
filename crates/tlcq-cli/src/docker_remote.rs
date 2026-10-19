//! The laptop's VM calls through `docker [--context <c>] exec -i <runner> tlcq
//! vm <verb>` (design D6, D13): the payload on stdin, the reply on stdout,
//! within a deadline.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;

use crate::commit::CommitReply;
use crate::proto::{
    CancelRequest, FetchRequest, PreflightReply, PreflightRequest, ResumeReply, ResumeRequest,
    VmJob,
};
use crate::remote::{CallError, Remote};

/// What `docker` prints when it never reached the runner: the daemon, the ssh
/// hop to it, or the runner container. Lower case, matched ignoring case: Docker
/// and podman word these differently (podman: "no such container", "container
/// state improper" for a stopped one).
const UNREACHED: [&str; 9] = [
    "cannot connect to the docker daemon",
    "error during connect",
    "no such container",
    "is not running",
    "container state improper",
    "ssh: ",
    "connection refused",
    "could not resolve hostname",
    "context not found",
];

/// What every failure of a runner's verb starts with: the call reached it.
pub const RUNNER_ERROR: &str = "tlcq vm";

/// The runner, reached with the `docker` CLI.
#[derive(Debug, Clone)]
pub struct DockerRemote {
    /// The `docker` binary.
    pub program: PathBuf,
    /// The docker context (`None`: the current one).
    pub context: Option<String>,
    /// The runner's container.
    pub container: String,
    /// The deadline of one call.
    pub deadline: Duration,
    /// How long a call that changes nothing the laptop relies on (preflight,
    /// status, ls, fetch) waits for its reply's first byte before the VM counts
    /// as unreachable. A commit or cancel never does: one cut short may have
    /// been applied, so its outcome is unknown, not unreached.
    pub connect: Duration,
}

impl DockerRemote {
    /// Call `verb` with `payload`, and parse its reply; `connect` as in
    /// [`DockerRemote::call_raw`].
    ///
    /// # Errors
    /// `Unreachable` if `docker` never reached the runner; else `Failed`.
    pub fn call<T: DeserializeOwned>(
        &self,
        verb: &str,
        payload: &[u8],
        connect: Option<Duration>,
    ) -> Result<T, CallError> {
        let out = self.call_raw(verb, payload, connect)?;
        serde_json::from_slice(&out).map_err(|e| CallError::Failed(format!("the reply: {e}")))
    }

    /// Call `verb` with `payload`; its reply's bytes. With `connect`, a reply
    /// whose first byte has not come by then is the VM unreachable.
    ///
    /// # Errors
    /// `Unreachable` if `docker` never reached the runner; else `Failed`.
    pub fn call_raw(
        &self,
        verb: &str,
        payload: &[u8],
        connect: Option<Duration>,
    ) -> Result<Vec<u8>, CallError> {
        let mut cmd = Command::new(&self.program);
        if let Some(c) = &self.context {
            cmd.args(["--context", c]);
        }
        cmd.args(["exec", "-i", &self.container, "tlcq", "vm", verb])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| CallError::Unreachable(e.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CallError::Failed("no stdin".to_owned()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| CallError::Failed("no stdout".to_owned()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| CallError::Failed("no stderr".to_owned()))?;
        let payload = payload.to_vec();
        let writer = std::thread::spawn(move || stdin.write_all(&payload));
        let heard = Arc::new(AtomicBool::new(false));
        let told = Arc::clone(&heard);
        let reader = std::thread::spawn(move || {
            let (mut out, mut err) = (Vec::new(), String::new());
            let mut first = [0; 1];
            let read = stdout
                .read(&mut first)
                .and_then(|n| {
                    told.store(true, Ordering::Relaxed);
                    out.extend_from_slice(first.get(..n).unwrap_or_default());
                    stdout.read_to_end(&mut out)
                })
                .and(stderr.read_to_string(&mut err));
            read.map(|_| (out, err))
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| CallError::Failed(e.to_string()))?
            {
                break status;
            }
            let silent = !heard.load(Ordering::Relaxed);
            if let Some(connect) = connect.filter(|c| silent && start.elapsed() > *c) {
                stop(&mut child);
                return Err(CallError::Unreachable(format!(
                    "no answer within {connect:?}"
                )));
            }
            if start.elapsed() > self.deadline {
                stop(&mut child);
                return Err(CallError::Failed(format!(
                    "no reply within {:?}",
                    self.deadline
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // A failed write is the runner closing stdin early; its reply says why.
        let _written = writer.join();
        let (out, err) = reader
            .join()
            .map_err(|panic| CallError::Failed(format!("reading the reply: {panic:?}")))?
            .map_err(|e| CallError::Failed(e.to_string()))?;
        if !status.success() {
            let err = err.trim().to_owned();
            let lower = err.to_lowercase();
            let reached = err.contains(RUNNER_ERROR);
            return Err(if !reached && UNREACHED.iter().any(|p| lower.contains(p)) {
                CallError::Unreachable(err)
            } else {
                CallError::Failed(err)
            });
        }
        Ok(out)
    }
}

/// Stop a call's `docker`.
fn stop(child: &mut Child) {
    let stopped = child.kill();
    stopped.unwrap_or_else(|e| eprintln!("stopping the VM call: {e}"));
}

impl Remote for DockerRemote {
    fn preflight(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError> {
        let payload = serde_json::to_vec(req).map_err(|e| CallError::Failed(e.to_string()))?;
        self.call("preflight", &payload, Some(self.connect))
    }

    fn status(&mut self, req: &PreflightRequest) -> Result<PreflightReply, CallError> {
        let payload = serde_json::to_vec(req).map_err(|e| CallError::Failed(e.to_string()))?;
        self.call("status", &payload, Some(self.connect))
    }

    fn commit(&mut self, wire: &[u8]) -> Result<CommitReply, CallError> {
        self.call("commit", wire, None)
    }

    fn ls(&mut self) -> Result<BTreeMap<String, VmJob>, CallError> {
        self.call("ls", b"", Some(self.connect))
    }

    fn cancel(&mut self, req: &CancelRequest) -> Result<VmJob, CallError> {
        let payload = serde_json::to_vec(req).map_err(|e| CallError::Failed(e.to_string()))?;
        self.call("cancel", &payload, None)
    }

    fn resume(&mut self, req: &ResumeRequest) -> Result<ResumeReply, CallError> {
        let payload = serde_json::to_vec(req).map_err(|e| CallError::Failed(e.to_string()))?;
        self.call("resume", &payload, None)
    }

    fn fetch(&mut self, req: &FetchRequest) -> Result<Vec<u8>, CallError> {
        let payload = serde_json::to_vec(req).map_err(|e| CallError::Failed(e.to_string()))?;
        self.call_raw("fetch", &payload, Some(self.connect))
    }
}

#[cfg(test)]
#[path = "docker_remote_tests.rs"]
mod tests;
