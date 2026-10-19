//! `tlcq`: queue TLA+ TLC jobs on the laptop or a dedicated VM.
//!
//! The library holds the logic; the binary is a thin shim over it. Design and
//! requirements live in `openspec/changes/tlc-queue/`.

pub mod batch;
pub mod commit;
pub mod config;
pub mod curve;
pub mod docker_remote;
pub mod engine;
pub mod entry;
pub mod estimates;
pub mod eta;
mod files;
pub mod history;
pub mod id;
pub mod job;
pub mod lanes;
pub mod laptop_runs;
pub mod manage;
pub mod must_fail;
pub mod options;
pub mod outcome;
pub mod plan;
pub mod proto;
pub mod refresh;
pub mod remote;
pub mod report;
pub mod runner;
pub mod snapshot;
pub mod status;
pub mod store;
pub mod submit;
pub mod supervise;
pub mod tlc;
pub mod worktree;
