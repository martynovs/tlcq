//! `tlcq submit`: layer the submission config, check the input, place the jobs
//! through the runner, and print the report.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use tlcq_cli::config::{self, Cfg, Config, Expect};
use tlcq_cli::submit::Submit;
use tlcq_cli::{batch, plan, report, submit, worktree};

use crate::cli::{LaptopArgs, SubmitArgs};
use crate::laptop;

/// Run `submit`; returns its exit code.
pub fn run(laptop: &LaptopArgs, args: &SubmitArgs) -> Result<u8> {
    let mut layered = config::load_dir(&args.dir)?.unwrap_or_default();
    if let Some(path) = &args.config {
        let (text, origin) = read_config(path)?;
        layered = layered.under(config::parse(&text, &origin)?);
    }
    let line = flags(args);
    // A flag beats every configuration's own entry, too (tlx-engine E1).
    let overrides = line.defaults();
    let sub = layered
        .under(line)
        .resolve(&args.dir)?
        .with_flags(&overrides)?;
    let origin = worktree::origin(&args.dir, std::env::var("TLCQ_AGENT").ok())?;
    let versions = laptop::versions(laptop)?;
    let batch = batch::build(
        &args.dir,
        &sub,
        &versions,
        crate::run::limits(&laptop.limits),
    )?;
    let store = laptop::store(laptop)?;
    let mut remote = laptop::remote(laptop);
    let s = Submit {
        cmd: plan::Command {
            jobs: &batch.jobs,
            worktree: origin.worktree,
            model_dir: origin.model_dir,
            label: origin.label,
            replace: sub.replace,
            local: sub.local,
            skip_max: sub.cfgs.is_none() && !sub.all,
        },
        versions,
        vm_on_timeout: sub.vm_on_timeout,
        slots: laptop.slots,
        token: worktree::token(),
    };
    let report = submit::submit(&store, &mut remote, &mut laptop::runs(laptop)?, &s)?;
    let mut cached = std::collections::BTreeMap::new();
    let skipped = report
        .plan
        .skipped
        .iter()
        .chain(batch.skipped.iter().map(|j| &j.id));
    for id in skipped {
        if let Some(result) = store.cached(id)? {
            cached.insert(id.clone(), result);
        }
    }
    print!(
        "{}",
        report::render(&report, &batch.jobs, &batch.skipped, &cached)
    );
    Ok(report.exit)
}

/// The command line as the top layer of the submission config.
fn flags(args: &SubmitArgs) -> Config {
    let set = |on: bool| on.then_some(true);
    let expect = if args.fast {
        Some(Expect::Fast)
    } else if args.slow {
        Some(Expect::Slow)
    } else {
        args.max.then_some(Expect::Max)
    };
    Config {
        cfgs: (!args.cfgs.is_empty()).then(|| args.cfgs.iter().cloned().map(Cfg::Name).collect()),
        all: set(args.all),
        replace: set(args.replace),
        local: set(args.local),
        vm_on_timeout: set(args.vm_on_timeout),
        expect,
        timeout: args.timeout.clone(),
        memory: args.memory.clone(),
        must_fail_only: set(args.must_fail_only),
        tlc_opts: (!args.tlc_opts.is_empty()).then(|| args.tlc_opts.clone()),
        engine: args.engine.map(Into::into),
        ..Config::default()
    }
}

/// A `--config` layer's text and its name for errors: a file, or `-` for stdin.
fn read_config(path: &Path) -> Result<(String, String)> {
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading the config from stdin")?;
        return Ok((text, "stdin".to_owned()));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok((text, path.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_file_is_read_with_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("submit.toml");
        std::fs::write(&path, "spec = \"M\"\n").unwrap();
        let (text, origin) = read_config(&path).unwrap();
        assert_eq!(
            (text.as_str(), origin),
            ("spec = \"M\"\n", path.display().to_string())
        );
    }
}
