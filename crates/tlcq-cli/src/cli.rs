//! Command-line interface, parsed by clap's derive.
//!
//! The agents' side: `tlcq submit`, `status`, `ls`, `cancel` and `fetch`. The
//! runner's side: `tlcq runner` (the long-lived loop), `tlcq exec` (an
//! attempt's supervisor, in its container) and `tlcq vm <verb>` (a VM verb, its
//! payload on stdin). Each is configured by flags or `TLCQ_*` variables.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tlcq_cli::engine::Engine;
use tlcq_cli::lanes::Settings;

/// Queue TLA+ TLC jobs on the laptop or a dedicated VM.
#[derive(Debug, Parser)]
#[command(name = "tlcq", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Cmd,
}

/// A command.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Verify a model directory's configurations and place them: reply with a
    /// report per job.
    Submit {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
        /// What to submit.
        #[command(flatten)]
        submit: SubmitArgs,
    },
    /// Report this agent's current jobs (the latest per configuration, and any
    /// older one still queued or running), or the named ones.
    Status {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
        /// Jobs to report, by id; none: this agent's current ones.
        ids: Vec<String>,
    },
    /// List every job, on the VM and the laptop.
    Ls {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
    },
    /// Cancel the run of a job this agent follows (else its latest run).
    Cancel {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
        /// The job.
        id: String,
    },
    /// Resume the paused run of a job this agent follows (else its latest run)
    /// from its kept checkpoint, first in the queue.
    Resume {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
        /// The job.
        id: String,
        /// The memory its attempts now ask for, as for `submit --memory`: for a
        /// run that died short of memory, up to what its status says one job gets.
        #[arg(long)]
        memory: Option<String>,
        /// Its new time cap, as for `submit --timeout` (`24h`): for a run that
        /// stopped at its cap, above the time it has run.
        #[arg(long)]
        timeout: Option<String>,
    },
    /// Write the logs and `result.json` of the run of a job this agent follows
    /// (else its latest run) under `./tmp/tlcq/<id>/<run>/<attempt>/`.
    Fetch {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
        /// The job.
        id: String,
    },
    /// Supervise one laptop job: started detached by the command that placed
    /// it, holding its job lock on stdin.
    #[command(hide = true)]
    Supervise {
        /// The laptop's settings.
        #[command(flatten)]
        laptop: LaptopArgs,
        /// The job.
        id: String,
        /// The run.
        run: String,
    },
    /// Run the runner: settle finished attempts and admit queued jobs, for ever.
    Runner {
        /// The runner's settings.
        #[command(flatten)]
        vm: VmArgs,
        /// Seconds between rounds.
        #[arg(long, default_value_t = 5)]
        poll_secs: u64,
        /// Stop after this many rounds (for tests).
        #[arg(long, hide = true)]
        rounds: Option<u64>,
        /// Serve the web page on this address, e.g. `0.0.0.0:8080` (D16).
        #[arg(long, env = "TLCQ_UI")]
        ui: Option<String>,
        /// The built page (`ui/dist`) the page's address serves.
        #[arg(long, env = "TLCQ_UI_DIR")]
        ui_dir: Option<PathBuf>,
    },
    /// Supervise one attempt: the process of its container.
    Exec {
        /// The runner's settings.
        #[command(flatten)]
        vm: VmArgs,
        /// The job.
        id: String,
        /// The run.
        run: String,
        /// The attempt.
        attempt: u32,
    },
    /// Serve one VM verb: its payload on stdin, its reply on stdout.
    Vm {
        /// The runner's settings.
        #[command(flatten)]
        vm: VmArgs,
        /// The verb.
        #[arg(value_enum)]
        verb: VerbArg,
    },
}

/// What `submit` submits (D1, D4): flags over `--config`, over the
/// directory's `tlcq.toml`.
#[derive(Debug, Clone, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one of submit's flags (D1)"
)]
pub struct SubmitArgs {
    /// The model directory.
    pub dir: PathBuf,
    /// The configurations to submit (default: every one but max ones).
    pub cfgs: Vec<String>,
    /// Include max configurations when none is named.
    #[arg(long)]
    pub all: bool,
    /// Expected under 5 minutes.
    #[arg(long, group = "expect")]
    pub fast: bool,
    /// Expected over 5 minutes.
    #[arg(long, group = "expect")]
    pub slow: bool,
    /// Expected over an hour.
    #[arg(long, group = "expect")]
    pub max: bool,
    /// Supersede this worktree's running older versions.
    #[arg(long)]
    pub replace: bool,
    /// A time cap replacing every lane cap, e.g. `90m` or `24h`.
    #[arg(long)]
    pub timeout: Option<String>,
    /// The least memory of every VM attempt, e.g. `64G` or `98304M` (powers
    /// of 1024; a bare number is MB). Ask only after a run's status says it
    /// needs more memory: above one slot's share, the run takes the whole VM
    /// and holds the P slot empty while it lives.
    #[arg(long)]
    pub memory: Option<String>,
    /// Run every job on the laptop, uncapped.
    #[arg(long)]
    pub local: bool,
    /// Move a laptop run that passes its cap to the VM.
    #[arg(long)]
    pub vm_on_timeout: bool,
    /// Check only the property each configuration's header says must fail.
    #[arg(long)]
    pub must_fail_only: bool,
    /// An allowlisted TLC option, e.g. `--tlc-opt=-deadlock`.
    #[arg(long = "tlc-opt", allow_hyphen_values = true)]
    pub tlc_opts: Vec<String>,
    /// The engine of every job, over the configurations' own `engine` (default:
    /// the config's, else tlx). Choose tlc for a `must fail` on a temporal
    /// property (tlx checks liveness only after the full search, TLC stops at
    /// the violation), a spec tlx refuses, or a result suspected wrong; add
    /// --replace while the tlx run is queued or running.
    #[arg(long, value_enum)]
    pub engine: Option<EngineArg>,
    /// A submission config in TOML: a file, or `-` for stdin.
    #[arg(long)]
    pub config: Option<PathBuf>,
}

/// An engine on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EngineArg {
    /// tlx, the Rust rewrite of TLC.
    Tlx,
    /// TLC.
    Tlc,
}

impl From<EngineArg> for Engine {
    fn from(e: EngineArg) -> Self {
        match e {
            EngineArg::Tlx => Engine::Tlx,
            EngineArg::Tlc => Engine::Tlc,
        }
    }
}

/// The laptop's settings (D13).
#[derive(Debug, Clone, Args)]
pub struct LaptopArgs {
    /// The state directory (default: `$XDG_STATE_HOME/tlcq`, else `~/.local/state/tlcq`).
    #[arg(long, env = "TLCQ_STATE")]
    pub state: Option<PathBuf>,
    /// The directory of the TLC jars, the same as the VM's (default:
    /// `$XDG_DATA_HOME/tlcq/jars`, else `~/.local/share/tlcq/jars`).
    #[arg(long, env = "TLCQ_JARS")]
    pub jars: Option<PathBuf>,
    /// The docker context the runner is in (empty: the current one).
    #[arg(long, env = "TLCQ_CONTEXT", default_value = "pve")]
    pub context: String,
    /// The runner's container.
    #[arg(long, env = "TLCQ_CONTAINER", default_value = "tlcq-runner")]
    pub container: String,
    /// The `docker` binary.
    #[arg(long, env = "TLCQ_DOCKER", default_value = "docker")]
    pub docker: PathBuf,
    /// The deadline of one VM call, in seconds.
    #[arg(long, env = "TLCQ_DEADLINE_SECS", default_value_t = 120)]
    pub deadline_secs: u64,
    /// Seconds a VM call that only reads waits for the first byte of its reply
    /// before the VM counts as unreachable.
    #[arg(long, env = "TLCQ_CONNECT_SECS", default_value_t = 10)]
    pub connect_secs: u64,
    /// The `java` binary for laptop runs.
    #[arg(long, env = "TLCQ_JAVA", default_value = "java")]
    pub java: PathBuf,
    /// The tlx binary, its `rev` file beside it, as `just install` puts them
    /// (default: `$XDG_DATA_HOME/tlcq/tlx/tlx`, else `~/.local/share/tlcq/tlx/tlx`).
    #[arg(long, env = "TLCQ_TLX")]
    pub tlx: Option<PathBuf>,
    /// Laptop jobs that run at once.
    #[arg(long, env = "TLCQ_SLOTS", default_value_t = 1)]
    pub slots: usize,
    /// TLC's workers on the laptop.
    #[arg(long, env = "TLCQ_WORKERS", default_value_t = 8)]
    pub workers: u64,
    /// TLC's heap on the laptop, in MB.
    #[arg(long, env = "TLCQ_HEAP_MB", default_value_t = 8192)]
    pub heap_mb: u64,
    /// Off-heap memory for TLC's fingerprints on the laptop, in MB (default:
    /// none, they stay on the heap).
    #[arg(long, env = "TLCQ_OFFHEAP_MB")]
    pub offheap_mb: Option<u64>,
    /// The cap on a laptop run that is not `--local`, in seconds of awake time.
    #[arg(long, env = "TLCQ_CAP_SECS", default_value_t = 300)]
    pub cap_secs: u64,
    /// Free disk, in GB, below which a laptop run waits.
    #[arg(long, env = "TLCQ_DISK_FLOOR_GB", default_value_t = 10)]
    pub disk_floor_gb: u64,
    /// Seconds between a laptop supervisor's looks at the slot, the disk and TLC.
    #[arg(long, env = "TLCQ_POLL_SECS", default_value_t = 5)]
    pub poll_secs: u64,
    /// The tarball limits.
    #[command(flatten)]
    pub limits: LimitArgs,
}

/// A VM verb on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerbArg {
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

/// The runner's settings (D9, D13).
#[derive(Debug, Clone, Args)]
pub struct VmArgs {
    /// The state directory: the state volume.
    #[arg(long, env = "TLCQ_STATE", default_value = "/state")]
    pub state: PathBuf,
    /// The directory of the pinned TLC jars.
    #[arg(long, env = "TLCQ_JARS", default_value = "/opt/tlcq/jars")]
    pub jars: PathBuf,
    /// The `java` binary.
    #[arg(long, env = "TLCQ_JAVA", default_value = "java")]
    pub java: PathBuf,
    /// The tlx binary, its `rev` file beside it.
    #[arg(long, env = "TLCQ_TLX", default_value = "/opt/tlcq/tlx/tlx")]
    pub tlx: PathBuf,
    /// A slot's memory share for tlx's `-maxmem`, in percent.
    #[arg(long, env = "TLCQ_TLX_MEM_PERCENT", default_value_t = 80)]
    pub tlx_mem_percent: u64,
    /// The `docker` binary (podman's socket through `DOCKER_HOST` on pve).
    #[arg(long, env = "TLCQ_DOCKER", default_value = "docker")]
    pub docker: PathBuf,
    /// The job image: the runner's own.
    #[arg(long, env = "TLCQ_IMAGE", default_value = "tlcq:latest")]
    pub image: String,
    /// The state volume's name, mounted into every attempt.
    #[arg(long, env = "TLCQ_VOLUME", default_value = "tlcq-state")]
    pub volume: String,
    /// The E slot's CPUs (pve's 16 efficiency cores), as `--cpuset-cpus` takes them.
    #[arg(long, env = "TLCQ_E_CPUS", default_value = "16-31")]
    pub e_cpus: String,
    /// The P slot's CPUs (pve's 8 performance cores, two threads each).
    #[arg(long, env = "TLCQ_P_CPUS", default_value = "0-15")]
    pub p_cpus: String,
    /// The VM's memory in MB (default: `--meminfo`'s total).
    #[arg(long, env = "TLCQ_MEMORY_MB")]
    pub memory_mb: Option<u64>,
    /// The VM's meminfo: its total memory, and what is free when an attempt
    /// starts.
    #[arg(long, env = "TLCQ_MEMINFO", default_value = "/proc/meminfo")]
    pub meminfo: PathBuf,
    /// Memory kept back, in MB.
    #[arg(long, env = "TLCQ_RESERVE_MB", default_value_t = 4096)]
    pub reserve_mb: u64,
    /// Free disk, in GB, below which nothing starts.
    #[arg(long, env = "TLCQ_DISK_FLOOR_GB", default_value_t = 20)]
    pub disk_floor_gb: u64,
    /// Attempts undone in a row before a run ends `error`.
    #[arg(long, env = "TLCQ_NEVER_STARTED", default_value_t = 3)]
    pub never_started: u32,
    /// Seconds a verb's payload may take to arrive.
    #[arg(long, env = "TLCQ_DEADLINE_SECS", default_value_t = 120)]
    pub deadline_secs: u64,
    /// A slot's memory share for TLC's heap, in percent.
    #[arg(long, env = "TLCQ_HEAP_PERCENT", default_value_t = 40)]
    pub heap_percent: u64,
    /// A slot's memory share for TLC's off-heap fingerprints, in percent.
    #[arg(long, env = "TLCQ_OFFHEAP_PERCENT", default_value_t = 50)]
    pub offheap_percent: u64,
    /// The runner sets it in each attempt's container: that attempt's memory
    /// cap, in MB, sized from what the VM had free when it started.
    #[arg(long, env = "TLCQ_SLOT_MEMORY_MB")]
    pub slot_memory_mb: Option<u64>,
    /// The lanes' timers, probe and caps.
    #[command(flatten)]
    pub lanes: LaneArgs,
    /// The tarball limits.
    #[command(flatten)]
    pub limits: LimitArgs,
}

/// The lanes' timers, the size probe and the caps (D9, D13).
#[derive(Debug, Clone, Args)]
pub struct LaneArgs {
    /// A short job still running after this much VM time becomes slow.
    #[arg(long, env = "TLCQ_SHORT_SECS", default_value_t = Settings::default().short_secs)]
    pub short_secs: u64,
    /// The size probe judges a slow job this long after it became slow.
    #[arg(long, env = "TLCQ_PROBE_SECS", default_value_t = Settings::default().probe_secs)]
    pub probe_secs: u64,
    /// The probe's window: its first sample this long before its second.
    #[arg(long, env = "TLCQ_PROBE_WINDOW_SECS", default_value_t = Settings::default().window_secs)]
    pub probe_window_secs: u64,
    /// Big when queue / distinct is at least this, in thousandths.
    #[arg(long, env = "TLCQ_PROBE_RATIO_PERMILLE", default_value_t = Settings::default().ratio_permille)]
    pub probe_ratio_permille: u64,
    /// Big when the queue grew by at least this share of distinct, in thousandths.
    #[arg(long, env = "TLCQ_PROBE_GROWTH_PERMILLE", default_value_t = Settings::default().growth_permille)]
    pub probe_growth_permille: u64,
    /// The slow lane's cap.
    #[arg(long, env = "TLCQ_SLOW_CAP_SECS", default_value_t = Settings::default().slow_cap_secs)]
    pub slow_cap_secs: u64,
    /// The max lane's cap.
    #[arg(long, env = "TLCQ_MAX_CAP_SECS", default_value_t = Settings::default().max_cap_secs)]
    pub max_cap_secs: u64,
}

/// The tarball limits (D13): the same on both machines.
#[derive(Debug, Clone, Args)]
pub struct LimitArgs {
    /// The largest a job's files may be, uncompressed, in MB.
    #[arg(long, env = "TLCQ_TARBALL_MB", default_value_t = 16)]
    pub tarball_mb: u64,
    /// The most files a job may hold.
    #[arg(long, env = "TLCQ_TARBALL_FILES", default_value_t = 2000)]
    pub tarball_files: usize,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    /// Validate the whole clap command tree (arg-name uniqueness, etc.) in one call.
    #[test]
    fn command_tree_is_valid() {
        super::Cli::command().debug_assert();
    }

    #[test]
    fn an_engine_flag_names_its_engine() {
        use tlcq_cli::engine::Engine;
        assert_eq!(Engine::from(super::EngineArg::Tlx), Engine::Tlx);
        assert_eq!(Engine::from(super::EngineArg::Tlc), Engine::Tlc);
    }
}
