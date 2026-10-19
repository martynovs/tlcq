//! Core logic: turn parsed args into the runner, the supervisor or a verb.

use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use tlcq_cli::engine::{self, Versions};
use tlcq_cli::id::{self, JARS};
use tlcq_cli::job::Limits;
use tlcq_cli::lanes::Settings;
use tlcq_cli::runner::docker::DockerCli;
use tlcq_cli::runner::exec::{Exec, Machine};
use tlcq_cli::runner::serve::{self, Deadline, SystemClock, Verb};
use tlcq_cli::runner::store::Store;
use tlcq_cli::runner::verbs::Runner;
use tlcq_cli::runner::{http, memory, ui};
use tlcq_cli::supervise::{AwakeClock, Supervise};

use crate::cli::{Cli, Cmd, LaneArgs, LimitArgs, VerbArg, VmArgs};

/// Run the CLI; returns the exit code.
pub fn run(cli: &Cli) -> Result<u8> {
    match &cli.command {
        Cmd::Submit { laptop, submit } => crate::submit_cmd::run(laptop, submit),
        Cmd::Status { laptop, ids } => crate::status_cmd::run(laptop, ids),
        Cmd::Ls { laptop } => crate::status_cmd::ls(laptop),
        Cmd::Cancel { laptop, id } => crate::status_cmd::cancel(laptop, id),
        Cmd::Resume {
            laptop,
            id,
            memory,
            timeout,
        } => crate::status_cmd::resume(laptop, id, memory.as_deref(), timeout.as_deref()),
        Cmd::Fetch { laptop, id } => crate::status_cmd::fetch(laptop, id),
        Cmd::Supervise { laptop, id, run } => {
            let store = crate::laptop::store(laptop)?;
            let mut supervise = Supervise {
                store: &store,
                java: laptop.java.clone(),
                jars: crate::laptop::jars(laptop)?,
                tlx: crate::laptop::tlx(laptop)?,
                versions: crate::laptop::versions(laptop)?,
                settings: crate::laptop::settings(laptop),
                clock: AwakeClock::new(),
                remote: crate::laptop::remote(laptop),
                token: tlcq_cli::worktree::token(),
            };
            supervise
                .run(id, run)
                .with_context(|| format!("laptop run {id} {run}"))?;
            Ok(0)
        }
        Cmd::Runner {
            vm,
            poll_secs,
            rounds,
            ui,
            ui_dir,
        } => {
            let mut runner = runner(vm)?;
            runner
                .store
                .set_stopping(false)
                .context("clearing the stopping mark")?;
            serve::stop_on_sigterm(runner.store.clone()).context("the SIGTERM handler")?;
            if let Some(addr) = ui {
                page(addr, ui_dir.clone(), self::runner(vm)?)?;
            }
            serve::run_loop(&mut runner, &SystemClock, *poll_secs, *rounds);
            Ok(0)
        }
        Cmd::Exec {
            vm,
            id,
            run,
            attempt,
        } => {
            let store = Store::open(&vm.state).context("the state directory")?;
            let exec = Exec {
                store: &store,
                machine: &machine(vm)?,
                settings: lane_settings(&vm.lanes),
                limits: limits(&vm.limits),
                clock: SystemClock,
                poll_secs: 5,
            };
            exec.run(id, run, *attempt)
                .with_context(|| format!("attempt {id} {run} {attempt}"))?;
            Ok(0)
        }
        Cmd::Vm { vm, verb } => {
            // Every failure of a verb names it `tlcq vm`, so the laptop can tell
            // the runner's own errors (its `docker` calls included) from a call
            // that never reached it.
            let serve_verb = || -> Result<()> {
                // One space first: the laptop's connect timeout waits for a byte
                // of the reply, which so means the runner was reached, however
                // long the verb then waits for the lock or works (D13). JSON
                // readers skip it.
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(b" ")?;
                stdout.flush()?;
                let mut runner = runner(vm)?;
                let stdin = BufReader::new(Deadline::new(std::io::stdin(), vm.deadline_secs));
                serve::serve(&mut runner, verb_of(*verb), stdin, &mut stdout).context("the verb")
            };
            serve_verb().context(tlcq_cli::docker_remote::RUNNER_ERROR)?;
            Ok(0)
        }
    }
}

/// The exit code of a failed command (D1): 3 when a VM call connected and then
/// failed, else 2 (a usage error, a refused input).
pub fn exit_code(err: &anyhow::Error) -> u8 {
    match err.downcast_ref::<tlcq_cli::snapshot::Error>() {
        Some(tlcq_cli::snapshot::Error::Preflight(_)) => 3,
        _ => 2,
    }
}

/// The verb a command-line verb names.
fn verb_of(verb: VerbArg) -> Verb {
    match verb {
        VerbArg::Preflight => Verb::Preflight,
        VerbArg::Commit => Verb::Commit,
        VerbArg::Status => Verb::Status,
        VerbArg::Cancel => Verb::Cancel,
        VerbArg::Resume => Verb::Resume,
        VerbArg::Ls => Verb::Ls,
        VerbArg::Fetch => Verb::Fetch,
        VerbArg::Reorder => Verb::Reorder,
    }
}

/// Serve the web page on `addr`, on a thread of its own: the built app in
/// `site`, its verbs through `runner` (D16).
fn page(addr: &str, site: Option<PathBuf>, mut runner: Runner<DockerCli>) -> Result<()> {
    let listener =
        std::net::TcpListener::bind(addr).with_context(|| format!("the page's address {addr}"))?;
    eprintln!(
        "tlcq runner: the page is at http://{}",
        listener.local_addr()?
    );
    let mut page = ui::Page::new(site);
    std::thread::spawn(move || {
        http::serve(&listener, |req| ui::handle(&mut runner, &mut page, req));
    });
    Ok(())
}

/// The runner, its attempts started through `docker`, each with the
/// runner's settings.
fn runner(vm: &VmArgs) -> Result<Runner<DockerCli>> {
    let machine = machine(vm)?;
    let lanes = &vm.lanes;
    let env = [
        ("STATE", "/state".to_owned()),
        ("E_CPUS", machine.e_cpus.clone()),
        ("P_CPUS", machine.p_cpus.clone()),
        ("MEMORY_MB", machine.memory_mb.to_string()),
        ("RESERVE_MB", machine.reserve_mb.to_string()),
        ("HEAP_PERCENT", machine.heap_percent.to_string()),
        ("OFFHEAP_PERCENT", machine.direct_percent.to_string()),
        ("TLX", machine.tlx.display().to_string()),
        ("TLX_MEM_PERCENT", machine.tlx_mem_percent.to_string()),
        ("TARBALL_MB", vm.limits.tarball_mb.to_string()),
        ("TARBALL_FILES", vm.limits.tarball_files.to_string()),
        ("SHORT_SECS", lanes.short_secs.to_string()),
        ("PROBE_SECS", lanes.probe_secs.to_string()),
        ("PROBE_WINDOW_SECS", lanes.probe_window_secs.to_string()),
        (
            "PROBE_RATIO_PERMILLE",
            lanes.probe_ratio_permille.to_string(),
        ),
        (
            "PROBE_GROWTH_PERMILLE",
            lanes.probe_growth_permille.to_string(),
        ),
        ("SLOW_CAP_SECS", lanes.slow_cap_secs.to_string()),
        ("MAX_CAP_SECS", lanes.max_cap_secs.to_string()),
    ]
    .map(|(k, v)| (format!("TLCQ_{k}"), v))
    .to_vec();
    Ok(Runner {
        store: Store::open(&vm.state).context("the state directory")?,
        containers: DockerCli {
            program: vm.docker.clone(),
            image: vm.image.clone(),
            volume: vm.volume.clone(),
            machine,
            env,
            meminfo: vm.meminfo.clone(),
        },
        versions: Versions {
            jar_version: id::jar_version(&vm.jars)
                .with_context(|| format!("the jars in {}", vm.jars.display()))?,
            tlx_rev: engine::tlx_rev(&vm.tlx)
                .with_context(|| format!("the rev beside {}", vm.tlx.display()))?,
        },
        limits: limits(&vm.limits),
        never_started: vm.never_started,
        disk_floor: vm.disk_floor_gb.saturating_mul(1 << 30),
    })
}

/// The VM's resources and TLC's install.
fn machine(vm: &VmArgs) -> Result<Machine> {
    let memory_mb = match vm.memory_mb {
        Some(m) => m,
        None => std::fs::read_to_string(&vm.meminfo)
            .ok()
            .and_then(|m| memory::field_mb(&m, "MemTotal"))
            .with_context(|| {
                format!(
                    "the VM's memory: set --memory-mb (or TLCQ_MEMORY_MB) where {} is missing",
                    vm.meminfo.display()
                )
            })?,
    };
    Ok(Machine {
        java: vm.java.clone(),
        jars: JARS.iter().map(|j| vm.jars.join(j)).collect(),
        tlx: vm.tlx.clone(),
        tlx_mem_percent: vm.tlx_mem_percent,
        e_cpus: vm.e_cpus.clone(),
        p_cpus: vm.p_cpus.clone(),
        memory_mb,
        reserve_mb: vm.reserve_mb,
        heap_percent: vm.heap_percent,
        direct_percent: vm.offheap_percent,
        slot_memory_mb: vm.slot_memory_mb,
    })
}

/// The lanes' timers, probe and caps.
fn lane_settings(lanes: &LaneArgs) -> Settings {
    Settings {
        short_secs: lanes.short_secs,
        probe_secs: lanes.probe_secs,
        window_secs: lanes.probe_window_secs,
        ratio_permille: lanes.probe_ratio_permille,
        growth_permille: lanes.probe_growth_permille,
        slow_cap_secs: lanes.slow_cap_secs,
        max_cap_secs: lanes.max_cap_secs,
    }
}

/// The tarball limits.
pub fn limits(args: &LimitArgs) -> Limits {
    Limits {
        bytes: args.tarball_mb.saturating_mul(1 << 20),
        entries: args.tarball_files,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_has_its_verb() {
        let all = [
            (VerbArg::Preflight, Verb::Preflight),
            (VerbArg::Commit, Verb::Commit),
            (VerbArg::Status, Verb::Status),
            (VerbArg::Cancel, Verb::Cancel),
            (VerbArg::Resume, Verb::Resume),
            (VerbArg::Ls, Verb::Ls),
            (VerbArg::Fetch, Verb::Fetch),
            (VerbArg::Reorder, Verb::Reorder),
        ];
        for (arg, verb) in all {
            assert_eq!(verb_of(arg), verb);
        }
    }
}
