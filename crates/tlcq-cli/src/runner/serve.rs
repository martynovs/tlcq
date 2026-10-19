//! The runner's process side (design D9): each VM verb as `tlcq vm <verb>`,
//! its payload JSON on stdin and its reply JSON on stdout, and the long-lived
//! `tlcq runner` loop that settles gone attempts and admits.

use std::io::{BufRead, Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use signal_hook::consts::SIGTERM;
use signal_hook::iterator::Signals;

use super::admit::Move;
use super::exec::Clock;
use super::store::Store;
use super::verbs::{Containers, Runner};
use crate::proto::{CancelRequest, FetchRequest, PreflightRequest, ResumeRequest};

/// A VM verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// The preflight.
    Preflight,
    /// The commit.
    Commit,
    /// A poll.
    Status,
    /// A cancel.
    Cancel,
    /// A resume.
    Resume,
    /// Every job.
    Ls,
    /// A run's files.
    Fetch,
    /// A human's reorder.
    Reorder,
}

/// `reorder`'s request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReorderRequest {
    /// The job.
    pub id: String,
    /// Its queued run.
    pub run: String,
    /// Where it goes.
    pub to: Move,
}

/// A verb's payload, read on a thread of its own so its arrival has a
/// deadline (D13): a sender gone quiet (a laptop asleep mid-send) fails the
/// verb at the deadline instead of holding it for ever. Only the payload's
/// arrival is timed, never the verb's work, which starts once it has all come.
#[derive(Debug)]
pub struct Deadline {
    chunks: Receiver<std::io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    at: usize,
    end: Instant,
    secs: u64,
}

impl Deadline {
    /// Read `input` within `secs` seconds from now.
    pub fn new(mut input: impl Read + Send + 'static, secs: u64) -> Self {
        let (send, chunks) = std::sync::mpsc::sync_channel(4);
        std::thread::spawn(move || loop {
            let mut chunk = vec![0; 64 * 1024];
            let read = input.read(&mut chunk).map(|n| {
                chunk.truncate(n);
                chunk
            });
            let last = !matches!(&read, Ok(c) if !c.is_empty());
            if send.send(read).is_err() || last {
                return;
            }
        });
        Self {
            chunks,
            chunk: Vec::new(),
            at: 0,
            end: Instant::now() + Duration::from_secs(secs),
            secs,
        }
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at == self.chunk.len() {
            let left = self.end.saturating_duration_since(Instant::now());
            self.chunk = match self.chunks.recv_timeout(left) {
                Ok(chunk) => chunk?,
                Err(RecvTimeoutError::Disconnected) => Vec::new(),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("the payload did not arrive within {} s", self.secs),
                    ))
                }
            };
            self.at = 0;
        }
        let mut rest = self.chunk.get(self.at..).unwrap_or_default();
        let n = rest.read(buf)?;
        self.at += n;
        Ok(n)
    }
}

/// Serve one verb: read its payload from `input`, apply it, write the reply.
///
/// # Errors
/// If the payload is not the verb's, or the verb fails.
pub fn serve<C: Containers>(
    runner: &mut Runner<C>,
    verb: Verb,
    mut input: impl BufRead,
    out: &mut impl Write,
) -> std::io::Result<()> {
    if verb == Verb::Commit {
        // Streamed: each tarball's length is checked before it is read.
        let answer = runner.commit(input).map_err(std::io::Error::other)?;
        return reply(out, &answer);
    }
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    match verb {
        Verb::Preflight => reply(out, &runner.preflight(&parse::<PreflightRequest>(&bytes)?)?),
        Verb::Status => reply(out, &runner.status(&parse::<PreflightRequest>(&bytes)?)?),
        Verb::Cancel => reply(out, &runner.cancel(&parse::<CancelRequest>(&bytes)?)?),
        Verb::Resume => reply(out, &runner.resume_run(&parse::<ResumeRequest>(&bytes)?)?),
        Verb::Fetch => runner.fetch(&parse::<FetchRequest>(&bytes)?, out),
        Verb::Reorder => {
            let req = parse::<ReorderRequest>(&bytes)?;
            reply(out, &runner.reorder(&req.id, &req.run, req.to)?)
        }
        Verb::Ls | Verb::Commit => reply(out, &runner.ls()?),
    }
}

/// A payload, parsed.
fn parse<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> std::io::Result<T> {
    serde_json::from_slice(bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write a reply as one line of JSON.
fn reply(out: &mut impl Write, value: &impl Serialize) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    out.write_all(b"\n")
}

/// The runner's loop: a start round that also re-sends cancel stops, then a
/// round every `poll_secs`, `rounds` times in all (`None`: for ever). A failed
/// round is reported and the loop goes on.
pub fn run_loop<C: Containers>(
    runner: &mut Runner<C>,
    clock: &impl Clock,
    poll_secs: u64,
    rounds: Option<u64>,
) {
    let mut done = 0;
    while rounds.is_none_or(|r| done < r) {
        if let Err(e) = runner.tick(done == 0) {
            eprintln!("runner round: {e}");
        }
        done += 1;
        clock.sleep(poll_secs);
    }
}

/// On SIGTERM (a shutdown or a restart), mark the runner stopping, so no
/// admission in any process starts a job only for it to be killed (D10), and
/// exit. Job containers outlive the runner; its next start settles and admits.
///
/// # Errors
/// If the handler cannot be installed.
pub fn stop_on_sigterm(store: Store) -> std::io::Result<()> {
    let mut signals = Signals::new([SIGTERM])?;
    std::thread::spawn(move || {
        if signals.forever().next().is_some() {
            if let Err(e) = store.set_stopping(true) {
                eprintln!("tlcq runner: marking it stopping: {e}");
            }
            std::process::exit(0);
        }
    });
    Ok(())
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn sleep(&self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
