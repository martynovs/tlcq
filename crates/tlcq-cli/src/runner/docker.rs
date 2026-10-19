//! The attempt containers through the `docker` CLI (design D10, D12): Docker
//! locally, or podman's Docker-compatible socket on pve (`DOCKER_HOST`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Output};

use super::admit::Shares;
use super::exec::Machine;
use super::memory;
use super::state::{Attempt, Run, Slot};
use super::verbs::Containers;

/// The label every attempt container carries, so `running` finds only them.
pub const LABEL: &str = "tlcq.attempt";

/// Attempt containers run with the `docker` CLI.
#[derive(Debug, Clone)]
pub struct DockerCli {
    /// The `docker` binary.
    pub program: PathBuf,
    /// The job image: the runner's own.
    pub image: String,
    /// The state volume, mounted at `/state` in every attempt.
    pub volume: String,
    /// The VM, for each slot's CPUs and memory.
    pub machine: Machine,
    /// Environment every attempt gets, so `tlcq exec` sees the runner's settings.
    pub env: Vec<(String, String)>,
    /// The VM's `/proc/meminfo`, for what is free when an attempt starts.
    pub meminfo: PathBuf,
}

impl DockerCli {
    /// Run `docker` with `args`.
    fn docker(&self, args: &[String]) -> std::io::Result<Output> {
        Command::new(&self.program).args(args).output()
    }

    /// Run `docker` with `args`, best effort: a failure is reported, not fatal
    /// (the container may be gone already).
    fn best_effort(&self, args: &[String]) {
        match self.docker(args) {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "docker {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => eprintln!("docker {}: {e}", args.join(" ")),
        }
    }

    /// Run `docker` with `args`, its failure as its error output.
    fn checked(&self, args: &[&str]) -> Result<(), String> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let out = self.docker(&args).map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    /// `docker stats` of `names`, one `used / cap` line each; `None` when none
    /// is named or it cannot be asked.
    fn stats(&self, names: &[String]) -> Option<String> {
        if names.is_empty() {
            return None;
        }
        let mut args = ["stats", "--no-stream", "--format", "{{.MemUsage}}"]
            .map(str::to_owned)
            .to_vec();
        args.extend(names.iter().cloned());
        match self.docker(&args) {
            Ok(out) if out.status.success() => {
                Some(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            _ => None,
        }
    }

    /// The room, in MB, the running attempts may still grow into, from
    /// `docker stats`, the `skip`ped ones left out (frozen: they cannot grow);
    /// nothing when none runs or it cannot be asked.
    fn promised_mb(&self, skip: &BTreeSet<String>) -> u64 {
        let names: Vec<String> = match self.running() {
            Ok(names) => names.difference(skip).cloned().collect(),
            Err(_) => return 0,
        };
        self.stats(&names).map_or(0, |s| memory::promised_mb(&s))
    }

    /// The attempt containers in one of `statuses`.
    fn listed(&self, statuses: &[&str]) -> std::io::Result<BTreeSet<String>> {
        let mut args: Vec<String> = ["ps", "--all"].map(str::to_owned).to_vec();
        for s in statuses {
            args.extend(["--filter".to_owned(), format!("status={s}")]);
        }
        args.extend([
            "--filter".to_owned(),
            format!("label={LABEL}"),
            "--format".to_owned(),
            "{{.Names}}".to_owned(),
        ]);
        let out = self.docker(&args)?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "docker ps: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// The VM's free memory now, in MB, if `/proc/meminfo` can be read.
    fn available_mb(&self) -> Option<u64> {
        std::fs::read_to_string(&self.meminfo)
            .ok()
            .and_then(|m| memory::field_mb(&m, "MemAvailable"))
    }
}

impl Containers for DockerCli {
    fn running(&self) -> std::io::Result<BTreeSet<String>> {
        // Not plain `ps`: podman lists only running containers there, so a
        // paused one would be settled as gone and removed.
        self.listed(&["running", "paused"])
    }

    fn paused(&self) -> std::io::Result<BTreeSet<String>> {
        self.listed(&["paused"])
    }

    /// The slot's share, cut to what is free now; when the VM's free memory
    /// cannot be read, the share alone.
    fn memory(&self, slot: Slot, alone: bool, frozen: &BTreeSet<String>) -> Option<u64> {
        let (_, ceiling) = self.machine.resources(slot, alone);
        let Some(available) = self.available_mb() else {
            return Some(ceiling);
        };
        memory::cap_mb(
            ceiling,
            available,
            self.promised_mb(frozen),
            self.machine.reserve_mb,
        )
    }

    fn shares(&self) -> Shares {
        Shares {
            slot_mb: self.machine.resources(Slot::E, false).1,
            whole_mb: self.machine.resources(Slot::E, true).1,
        }
    }

    fn workers(&self, slot: Slot) -> u64 {
        self.machine.resources(slot, false).0
    }

    fn free_mb(&self) -> Option<u64> {
        self.available_mb()
    }

    fn room_mb(&self) -> Option<u64> {
        let promised = self.promised_mb(&BTreeSet::new());
        Some(
            self.available_mb()?
                .saturating_sub(self.machine.reserve_mb + promised),
        )
    }

    fn start(
        &mut self,
        name: &str,
        id: &str,
        run: &Run,
        attempt: &Attempt,
        memory: u64,
    ) -> Result<(), String> {
        let mut args: Vec<String> = ["run", "--detach", "--init", "--name", name]
            .map(str::to_owned)
            .to_vec();
        args.extend(["--label".to_owned(), format!("{LABEL}={name}")]);
        args.extend([
            "--cpuset-cpus".to_owned(),
            self.machine.cpus(attempt.slot).to_owned(),
            "--memory".to_owned(),
            format!("{memory}m"),
        ]);
        args.extend(["--volume".to_owned(), format!("{}:/state", self.volume)]);
        for (k, v) in &self.env {
            args.extend(["--env".to_owned(), format!("{k}={v}")]);
        }
        // The cap is the attempt's memory: TLC's heap and off-heap come from it.
        args.extend(["--env".to_owned(), format!("TLCQ_SLOT_MEMORY_MB={memory}")]);
        args.extend([self.image.clone(), "tlcq".to_owned(), "exec".to_owned()]);
        args.extend([id.to_owned(), run.name.clone(), attempt.number.to_string()]);
        let out = self.docker(&args).map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        }
    }

    fn pause(&mut self, name: &str) -> Result<(), String> {
        self.checked(&["pause", name])
    }

    fn unpause(&mut self, name: &str) -> Result<(), String> {
        self.checked(&["unpause", name])
    }

    fn move_to(&mut self, name: &str, slot: Slot) -> Result<(), String> {
        let cpus = self.machine.cpus(slot).to_owned();
        self.checked(&["update", "--cpuset-cpus", &cpus, name])
    }

    fn stop(&mut self, name: &str) {
        self.best_effort(&["kill".to_owned(), name.to_owned()]);
    }

    fn remove(&mut self, name: &str) {
        self.best_effort(&["rm".to_owned(), "--force".to_owned(), name.to_owned()]);
    }

    fn prune_images(&mut self) {
        self.best_effort(&["image", "prune", "--force"].map(str::to_owned));
    }
}

#[cfg(test)]
#[path = "docker_tests.rs"]
mod tests;
