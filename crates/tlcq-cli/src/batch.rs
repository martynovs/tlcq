//! A command's jobs, built and checked before it takes the placement lock
//! (design D6, input checks).
//!
//! Each configuration becomes a job: its bytes (rewritten by `--must-fail-only`),
//! the directory's modules, its id and history keys. Jobs that share an id are
//! merged into one, with one entry per configuration name. With no
//! configurations named and no `--all`, jobs whose every name is max by its
//! suffix are set aside for the skip-max listing. Any failure here exits 2 with
//! nothing written.

use std::path::{Path, PathBuf};

use crate::config::{Job as Cfg, JobOptions, Submission};
use crate::engine::{Engine, Versions};
use crate::history::{self, Expected, Keys};
use crate::job::{self, Contents, Limits};
use crate::{id, must_fail};

/// One configuration name of a job, with what is particular to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// The configuration's file name.
    pub name: String,
    /// Its history keys.
    pub keys: Keys,
    /// Its options from the submission.
    pub options: JobOptions,
}

/// A job: one id, its files, and every configuration name that gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The content-hash id.
    pub id: String,
    /// The engine it runs on, which the id includes.
    pub engine: Engine,
    /// The manifest the id hashes.
    pub manifest: String,
    /// Its files.
    pub contents: Contents,
    /// Its configuration names, in submit order.
    pub names: Vec<Named>,
}

impl Job {
    /// Whether every name's suffix says max.
    pub fn max_by_suffix(&self) -> bool {
        self.names
            .iter()
            .all(|n| history::suffix(&n.name) == Some(Expected::Max))
    }

    /// Whether its configuration must fail a temporal property, which tlx
    /// finds only after the whole search (tlx-engine E6).
    pub fn temporal_must_fail(&self) -> bool {
        self.contents
            .files()
            .iter()
            .find(|f| f.path == job::CFG)
            .and_then(|f| std::str::from_utf8(&f.bytes).ok())
            .is_some_and(must_fail::temporal)
    }

    /// The largest memory request of its names, in MB (job-memory M1).
    pub fn memory_mb(&self) -> Option<u64> {
        self.names.iter().filter_map(|n| n.options.memory_mb).max()
    }
}

/// A command's jobs, and those set aside as max.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// The jobs to place, in submit order.
    pub jobs: Vec<Job>,
    /// The jobs skipped as max by their suffix (listed, not placed).
    pub skipped: Vec<Job>,
}

/// Why a command's input was refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configuration named that is not in the directory.
    #[error("{0}: no such configuration in the model directory")]
    NoSuchCfg(String),
    /// A configuration named twice in one command.
    #[error("{0}: named twice")]
    NamedTwice(String),
    /// The directory or a configuration could not be read.
    #[error("{path}: {source}")]
    Read {
        /// What was being read.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// An expected duration given for every configuration without naming any.
    #[error("an expected duration (--fast, --slow, --max or `expect`) needs named configurations or --all")]
    DurationWithoutNames,
    /// A `--must-fail-only` rewrite that could not be done.
    #[error("{name}: {source}")]
    MustFail {
        /// The configuration.
        name: String,
        /// Why.
        source: must_fail::Error,
    },
    /// A job the VM would refuse.
    #[error(transparent)]
    Job(#[from] job::Error),
    /// A tlx job on a laptop without tlx.
    #[error("{0}: tlx is not installed (run `just install`), or submit it with --engine tlc")]
    NoTlx(String),
}

/// Build the jobs of the submission `sub` for the model directory `dir`, each
/// with its engine's identity from the laptop's `versions`.
pub fn build(
    dir: &Path,
    sub: &Submission,
    versions: &Versions,
    limits: Limits,
) -> Result<Batch, Error> {
    let named = sub.cfgs.is_some();
    if !named && !sub.all && sub.defaults.expect.is_some() {
        return Err(Error::DurationWithoutNames);
    }
    let cfgs = match &sub.cfgs {
        Some(cfgs) => cfgs.clone(),
        None => every_cfg(dir)?
            .into_iter()
            .map(|name| Cfg {
                name,
                options: sub.defaults.clone(),
            })
            .collect(),
    };
    let mut seen = std::collections::BTreeSet::new();
    if let Some(twice) = cfgs.iter().find(|c| !seen.insert(c.name.as_str())) {
        return Err(Error::NamedTwice(twice.name.clone()));
    }
    let modules = job::modules_of(dir, &sub.extends)?;
    let mut jobs: Vec<Job> = Vec::new();
    for cfg in cfgs {
        let engine = cfg.options.engine;
        let identity = versions
            .identity(engine)
            .ok_or_else(|| Error::NoTlx(cfg.name.clone()))?;
        let (contents, manifest) = build_one(dir, sub, &cfg, &modules, &identity, limits)?;
        let id = id::id(&manifest);
        let named = Named {
            keys: Keys::new(
                engine,
                &manifest,
                &contents,
                &cfg.options.tlc_opts,
                &sub.spec,
                &cfg.name,
            ),
            name: cfg.name,
            options: cfg.options,
        };
        match jobs.iter_mut().find(|j| j.id == id) {
            Some(job) => job.names.push(named),
            None => jobs.push(Job {
                id,
                engine,
                manifest,
                contents,
                names: vec![named],
            }),
        }
    }
    let (skipped, jobs) = if named || sub.all {
        (Vec::new(), jobs)
    } else {
        jobs.into_iter().partition(Job::max_by_suffix)
    };
    Ok(Batch { jobs, skipped })
}

/// One configuration's contents and manifest.
fn build_one(
    dir: &Path,
    sub: &Submission,
    cfg: &Cfg,
    modules: &[job::File],
    identity: &str,
    limits: Limits,
) -> Result<(Contents, String), Error> {
    let path = dir.join(&cfg.name);
    if !path.is_file() {
        return Err(Error::NoSuchCfg(cfg.name.clone()));
    }
    let bytes = std::fs::read(&path).map_err(|source| Error::Read { path, source })?;
    let bytes = if cfg.options.must_fail_only {
        rewrite(&cfg.name, bytes)?
    } else {
        bytes
    };
    let contents = job::with_cfg(modules, &bytes, limits)?;
    let manifest = id::manifest(&sub.spec, &contents, &cfg.options.tlc_opts, identity);
    Ok((contents, manifest))
}

/// The `--must-fail-only` rewrite of a configuration with a `must fail` header;
/// one without the header is left as it is.
fn rewrite(name: &str, bytes: Vec<u8>) -> Result<Vec<u8>, Error> {
    let Ok(text) = String::from_utf8(bytes.clone()) else {
        return Ok(bytes);
    };
    match must_fail::expectation(&text) {
        None => Ok(bytes),
        Some(target) => must_fail::rewrite(&text, &target)
            .map(String::into_bytes)
            .map_err(|source| Error::MustFail {
                name: name.to_owned(),
                source,
            }),
    }
}

/// Every `*.cfg` in `dir`, by name.
fn every_cfg(dir: &Path) -> Result<Vec<String>, Error> {
    let read = |source| Error::Read {
        path: dir.to_path_buf(),
        source,
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(read)? {
        let name = entry
            .map_err(read)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if Path::new(&name).extension().is_some_and(|e| e == "cfg") && !name.starts_with('.') {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Expect;

    const BUG: &str = "\\* Bug: must fail Safe.\nINIT Init\nNEXT Next\nINVARIANTS Safe Other\n";

    /// A model directory with `M.tla` and the given configurations.
    fn model(cfgs: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("M.tla"), "---- MODULE M ----\n====\n").unwrap();
        for (name, text) in cfgs {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    fn sub(cfgs: Option<Vec<Cfg>>) -> Submission {
        Submission {
            spec: "M".to_owned(),
            extends: Vec::new(),
            cfgs,
            defaults: JobOptions::default(),
            all: false,
            replace: false,
            local: false,
            vm_on_timeout: false,
        }
    }

    fn cfg(name: &str, options: JobOptions) -> Cfg {
        Cfg {
            name: name.to_owned(),
            options,
        }
    }

    /// A laptop with both engines.
    fn versions() -> Versions {
        Versions {
            jar_version: "J".to_owned(),
            tlx_rev: Some("R".to_owned()),
        }
    }

    #[test]
    fn each_job_carries_its_engine_and_a_tlx_job_needs_tlx() {
        let dir = model(&[("A.cfg", "INIT Init\n")]);
        let on = |engine| {
            sub(Some(vec![cfg(
                "A.cfg",
                JobOptions {
                    engine,
                    ..JobOptions::default()
                },
            )]))
        };
        let tlx = build(dir.path(), &on(Engine::Tlx), &versions(), Limits::default()).unwrap();
        let tlc = build(dir.path(), &on(Engine::Tlc), &versions(), Limits::default()).unwrap();
        assert_eq!(tlx.jobs[0].engine, Engine::Tlx);
        assert!(tlx.jobs[0].manifest.ends_with("engine tlx R\n"));
        assert_eq!(tlc.jobs[0].engine, Engine::Tlc);
        assert!(tlc.jobs[0].manifest.ends_with("engine tlc J\n"));
        assert_ne!(tlx.jobs[0].id, tlc.jobs[0].id);
        let no_tlx = Versions {
            tlx_rev: None,
            ..versions()
        };
        assert!(matches!(
            build(dir.path(), &on(Engine::Tlx), &no_tlx, Limits::default()),
            Err(Error::NoTlx(_))
        ));
        build(dir.path(), &on(Engine::Tlc), &no_tlx, Limits::default()).unwrap();
    }

    fn names(jobs: &[Job]) -> Vec<Vec<&str>> {
        jobs.iter()
            .map(|j| j.names.iter().map(|n| n.name.as_str()).collect())
            .collect()
    }

    #[test]
    fn every_cfg_but_the_max_ones_and_same_text_merged() {
        let dir = model(&[
            ("A.cfg", "INIT Init\n"),
            ("B.cfg", "INIT Init\n"),
            ("C.cfg", "NEXT Next\n"),
            ("D_max.cfg", "INIT Big\n"),
            ("notes.txt", ""),
        ]);
        let b = build(dir.path(), &sub(None), &versions(), Limits::default()).unwrap();
        assert_eq!(names(&b.jobs), [vec!["A.cfg", "B.cfg"], vec!["C.cfg"]]);
        assert_eq!(names(&b.skipped), [vec!["D_max.cfg"]]);
        assert_eq!(b.jobs.first().unwrap().id.len(), 64);
    }

    #[test]
    fn a_memory_request_leaves_the_id_alone_and_a_merged_job_takes_the_largest() {
        let dir = model(&[("A.cfg", "INIT Init\n"), ("B.cfg", "INIT Init\n")]);
        let asking = |a: Option<u64>, b: Option<u64>| {
            let o = |memory_mb| JobOptions {
                memory_mb,
                ..JobOptions::default()
            };
            sub(Some(vec![cfg("A.cfg", o(a)), cfg("B.cfg", o(b))]))
        };
        let plain = build(
            dir.path(),
            &asking(None, None),
            &versions(),
            Limits::default(),
        )
        .unwrap();
        let more = build(
            dir.path(),
            &asking(Some(8192), Some(65_536)),
            &versions(),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(more.jobs[0].id, plain.jobs[0].id);
        assert_eq!(more.jobs[0].memory_mb(), Some(65_536));
        assert_eq!(plain.jobs[0].memory_mb(), None);
    }

    #[test]
    fn a_merged_job_is_skipped_only_if_every_name_is_max() {
        let dir = model(&[("A_max.cfg", "INIT Init\n"), ("B.cfg", "INIT Init\n")]);
        let b = build(dir.path(), &sub(None), &versions(), Limits::default()).unwrap();
        assert_eq!(names(&b.jobs), [vec!["A_max.cfg", "B.cfg"]]);
        assert_eq!(b.skipped, Vec::<Job>::new());
    }

    #[test]
    fn all_and_named_skip_nothing() {
        let dir = model(&[("D_max.cfg", "INIT Big\n")]);
        let mut all = sub(None);
        all.all = true;
        assert_eq!(
            build(dir.path(), &all, &versions(), Limits::default())
                .unwrap()
                .jobs
                .len(),
            1
        );
        let named = sub(Some(vec![cfg("D_max.cfg", JobOptions::default())]));
        assert_eq!(
            build(dir.path(), &named, &versions(), Limits::default())
                .unwrap()
                .jobs
                .len(),
            1
        );
    }

    #[test]
    fn must_fail_only_rewrites_and_changes_the_id() {
        let dir = model(&[("Bug.cfg", BUG), ("Plain.cfg", "INIT Init\n")]);
        let only = JobOptions {
            must_fail_only: true,
            ..JobOptions::default()
        };
        let plain = build(
            dir.path(),
            &sub(Some(vec![cfg("Bug.cfg", JobOptions::default())])),
            &versions(),
            Limits::default(),
        )
        .unwrap();
        let b = build(
            dir.path(),
            &sub(Some(vec![
                cfg("Bug.cfg", only.clone()),
                cfg("Plain.cfg", only),
            ])),
            &versions(),
            Limits::default(),
        )
        .unwrap();
        let bug = b.jobs.first().unwrap();
        let cfg_bytes = &bug
            .contents
            .files()
            .iter()
            .find(|f| f.path == job::CFG)
            .unwrap()
            .bytes;
        assert!(
            String::from_utf8_lossy(cfg_bytes).contains("INVARIANTS Safe\nCHECK_DEADLOCK FALSE")
        );
        assert_ne!(bug.id, plain.jobs.first().unwrap().id);
        let untouched = &b.jobs[1]
            .contents
            .files()
            .iter()
            .find(|f| f.path == job::CFG)
            .unwrap()
            .bytes;
        assert_eq!(untouched.as_slice(), b"INIT Init\n");
    }

    #[test]
    fn refusals() {
        let dir = model(&[("Bug.cfg", "\\* must fail Nope\nINVARIANT Safe\n")]);
        let twice = sub(Some(vec![
            cfg("Bug.cfg", JobOptions::default()),
            cfg("Bug.cfg", JobOptions::default()),
        ]));
        assert!(matches!(
            build(dir.path(), &twice, &versions(), Limits::default()),
            Err(Error::NamedTwice(_))
        ));
        let missing = sub(Some(vec![cfg("Gone.cfg", JobOptions::default())]));
        assert!(matches!(
            build(dir.path(), &missing, &versions(), Limits::default()),
            Err(Error::NoSuchCfg(_))
        ));
        let only = JobOptions {
            must_fail_only: true,
            ..JobOptions::default()
        };
        let not_listed = sub(Some(vec![cfg("Bug.cfg", only)]));
        assert!(matches!(
            build(dir.path(), &not_listed, &versions(), Limits::default()),
            Err(Error::MustFail { .. })
        ));
        let mut blanket = sub(None);
        blanket.defaults.expect = Some(Expect::Max);
        assert!(matches!(
            build(dir.path(), &blanket, &versions(), Limits::default()),
            Err(Error::DurationWithoutNames)
        ));
        blanket.all = true;
        build(dir.path(), &blanket, &versions(), Limits::default()).unwrap();
        let tiny = Limits {
            bytes: 1,
            entries: 9,
        };
        assert!(matches!(
            build(dir.path(), &sub(None), &versions(), tiny),
            Err(Error::Job(_))
        ));
        assert!(matches!(
            build(
                &dir.path().join("nope"),
                &sub(None),
                &versions(),
                Limits::default()
            ),
            Err(Error::Read { .. })
        ));
    }

    #[test]
    fn a_cfg_that_is_not_utf8_is_left_as_it_is() {
        assert_eq!(rewrite("x.cfg", vec![0xff, 0xfe]).unwrap(), [0xff, 0xfe]);
    }
}
