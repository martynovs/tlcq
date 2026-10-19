//! The laptop's settings, resolved: its state directory, its jars, the runner
//! it calls, and its runs' supervisors (D7, D13).

use std::ffi::OsString;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use rustix::process::{kill_process_group, test_kill_process_group, Pid, Signal};
use tlcq_cli::docker_remote::DockerRemote;
use tlcq_cli::engine::{self, Versions};
use tlcq_cli::id::{self, JARS};
use tlcq_cli::laptop_runs::{LaptopRuns, Processes};
use tlcq_cli::runner::exec::Resources;
use tlcq_cli::store::Store;
use tlcq_cli::supervise::Settings;

use crate::cli::LaptopArgs;

/// The laptop's state directory.
fn state_dir(laptop: &LaptopArgs) -> Result<PathBuf> {
    dir_or(
        laptop.state.as_deref(),
        env("XDG_STATE_HOME"),
        ".local/state",
        "",
    )
}

/// The laptop's state directory, opened.
pub fn store(laptop: &LaptopArgs) -> Result<Store> {
    let dir = state_dir(laptop)?;
    Store::open(&dir).with_context(|| format!("the state directory {}", dir.display()))
}

/// The directory of the laptop's jars.
fn jars_dir(laptop: &LaptopArgs) -> Result<PathBuf> {
    dir_or(
        laptop.jars.as_deref(),
        env("XDG_DATA_HOME"),
        ".local/share",
        "jars",
    )
}

/// The laptop's jars, TLC's first.
pub fn jars(laptop: &LaptopArgs) -> Result<Vec<PathBuf>> {
    let dir = jars_dir(laptop)?;
    Ok(JARS.iter().map(|j| dir.join(j)).collect())
}

/// The laptop's tlx binary.
pub fn tlx(laptop: &LaptopArgs) -> Result<PathBuf> {
    if let Some(tlx) = &laptop.tlx {
        return Ok(tlx.clone());
    }
    Ok(dir_or(None, env("XDG_DATA_HOME"), ".local/share", "tlx")?.join("tlx"))
}

/// The laptop's engine versions: its jar version, and its tlx rev if tlx is
/// installed.
pub fn versions(laptop: &LaptopArgs) -> Result<Versions> {
    let jars = jars_dir(laptop)?;
    let tlx = tlx(laptop)?;
    Ok(Versions {
        jar_version: id::jar_version(&jars)
            .with_context(|| format!("the jars in {}", jars.display()))?,
        tlx_rev: engine::tlx_rev(&tlx)
            .with_context(|| format!("the rev beside {}", tlx.display()))?,
    })
}

/// The laptop runs' slot, resources and cap.
pub fn settings(laptop: &LaptopArgs) -> Settings {
    Settings {
        slots: laptop.slots,
        resources: Resources {
            workers: laptop.workers,
            heap_mb: laptop.heap_mb,
            direct_mb: laptop.offheap_mb,
            // tlx's whole budget is the heap TLC would have had (tlx-engine E3).
            tlx_mem_mb: laptop.heap_mb,
        },
        cap_secs: laptop.cap_secs,
        disk_floor: laptop.disk_floor_gb << 30,
        poll_secs: laptop.poll_secs,
        limits: crate::run::limits(&laptop.limits),
    }
}

/// The laptop's runs, each supervisor this binary as `tlcq supervise`.
pub fn runs(laptop: &LaptopArgs) -> Result<LaptopRuns<Spawner>> {
    Ok(LaptopRuns {
        processes: Spawner::new(laptop)?,
    })
}

/// Supervisors started as `tlcq supervise`, each in its own process group, so
/// it outlives the command that placed its job, and `cancel` kills it with TLC.
#[derive(Debug)]
pub struct Spawner {
    /// The binary.
    exe: PathBuf,
    /// Its arguments before the id and the run: every setting, resolved.
    args: Vec<OsString>,
}

impl Spawner {
    /// A spawner passing on `laptop`'s settings.
    fn new(laptop: &LaptopArgs) -> Result<Self> {
        let mut args: Vec<OsString> = vec!["supervise".into()];
        let mut flag = |name: &str, value: OsString| args.extend([name.into(), value]);
        flag("--state", state_dir(laptop)?.into());
        flag("--jars", jars_dir(laptop)?.into());
        flag("--context", laptop.context.clone().into());
        flag("--container", laptop.container.clone().into());
        flag("--docker", laptop.docker.clone().into());
        flag("--deadline-secs", laptop.deadline_secs.to_string().into());
        flag("--connect-secs", laptop.connect_secs.to_string().into());
        flag("--java", laptop.java.clone().into());
        flag("--tlx", tlx(laptop)?.into());
        flag("--slots", laptop.slots.to_string().into());
        flag("--workers", laptop.workers.to_string().into());
        flag("--heap-mb", laptop.heap_mb.to_string().into());
        if let Some(mb) = laptop.offheap_mb {
            flag("--offheap-mb", mb.to_string().into());
        }
        flag("--cap-secs", laptop.cap_secs.to_string().into());
        flag("--disk-floor-gb", laptop.disk_floor_gb.to_string().into());
        flag("--poll-secs", laptop.poll_secs.to_string().into());
        Ok(Self {
            exe: std::env::current_exe().context("this binary's path")?,
            args,
        })
    }
}

impl Processes for Spawner {
    fn spawn(&mut self, dir: &Path, id: &str, run: &str, lock: File) -> std::io::Result<u32> {
        let log = File::create(dir.join("supervisor.log"))?;
        let child = Command::new(&self.exe)
            .args(&self.args)
            .args([id, run])
            // The job lock, held for as long as the supervisor lives.
            .stdin(Stdio::from(lock))
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()?;
        Ok(child.id())
    }

    fn kill(&mut self, pid: u32) -> std::io::Result<()> {
        // Never 0 or 1: those name this process's own group, or every process.
        let pid = i32::try_from(pid)
            .ok()
            .filter(|p| *p > 1)
            .and_then(Pid::from_raw)
            .ok_or_else(|| std::io::Error::other(format!("{pid}: not a supervisor")))?;
        match kill_process_group(pid, Signal::KILL) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(e) => return Err(e.into()),
        }
        // TLC dies with it; wait (a while) until no process of the group is left.
        for _ in 0..200 {
            if test_kill_process_group(pid).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

/// The runner, through `docker`.
pub fn remote(laptop: &LaptopArgs) -> DockerRemote {
    DockerRemote {
        program: laptop.docker.clone(),
        context: (!laptop.context.is_empty()).then(|| laptop.context.clone()),
        container: laptop.container.clone(),
        deadline: Duration::from_secs(laptop.deadline_secs),
        connect: Duration::from_secs(laptop.connect_secs),
    }
}

/// The XDG base directory `xdg`, and `$HOME`, from the environment.
fn env(xdg: &str) -> (Option<OsString>, Option<OsString>) {
    (std::env::var_os(xdg), std::env::var_os("HOME"))
}

/// `given`, else `<xdg>/tlcq/<leaf>`, else `<home>/<fallback>/tlcq/<leaf>`.
fn dir_or(
    given: Option<&Path>,
    (xdg, home): (Option<OsString>, Option<OsString>),
    fallback: &str,
    leaf: &str,
) -> Result<PathBuf> {
    if let Some(dir) = given {
        return Ok(dir.to_path_buf());
    }
    let base = match xdg.filter(|v| !v.is_empty()) {
        Some(base) => PathBuf::from(base),
        None => PathBuf::from(home.context("$HOME is not set")?).join(fallback),
    };
    Ok(base.join("tlcq").join(leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_defaults_to_xdg_then_home() {
        let some = |s: &str| Some(OsString::from(s));
        let given = dir_or(Some(Path::new("/given")), (None, None), ".x", "jars").unwrap();
        assert_eq!(given, PathBuf::from("/given"));
        let xdg = dir_or(None, (some("/xdg"), some("/home")), ".local/share", "jars").unwrap();
        assert_eq!(xdg, PathBuf::from("/xdg/tlcq/jars"));
        let home = dir_or(None, (some(""), some("/home")), ".local/state", "").unwrap();
        assert_eq!(home, PathBuf::from("/home/.local/state/tlcq/"));
        dir_or(None, (None, None), ".local/state", "").unwrap_err();
    }
}
