//! The submission config (design D4).
//!
//! A submission is described in TOML, one schema wherever it comes from:
//!
//! ```toml
//! spec = "Juliet"
//! extends = ["../november"]   # other directories whose modules the spec reuses
//! replace = true
//! cfgs = [
//!   "JA.cfg",
//!   { name = "JA_10.cfg", must_fail_only = true },
//!   { name = "JA_08_max.cfg", timeout = "24h" },
//! ]
//! ```
//!
//! It comes in layers, each overriding the one before key by key: the model
//! directory's `tlcq.toml`, if there is one; then `--config <file>` or
//! `--config -` (stdin); then the command line. A configuration's own fields
//! override the top-level defaults. Configuration names on the command line
//! replace `cfgs`, each keeping the fields of the entry of that name below
//! (tlx-engine E1). The command line's flags then override every
//! configuration's own fields too ([`Submission::with_flags`]): a flag always wins.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::engine::Engine;
use crate::options;

/// The directory's defaults file.
pub const FILE: &str = "tlcq.toml";

/// How long a job is expected to run, when a submission says so (D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    /// Under 5 minutes (`--fast`).
    Fast,
    /// Over 5 minutes (`--slow`).
    Slow,
    /// Over an hour (`--max`).
    Max,
}

/// One layer of a submission config; every key optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The spec module's name, without `.tla`.
    pub spec: Option<String>,
    /// Directories whose modules the spec reuses, relative to the model directory.
    pub extends: Option<Vec<PathBuf>>,
    /// The configurations to submit; none means every one in the directory.
    pub cfgs: Option<Vec<Cfg>>,
    /// Include max configurations when none is named.
    pub all: Option<bool>,
    /// Supersede this worktree's running older versions.
    pub replace: Option<bool>,
    /// Run every job on the laptop, uncapped.
    pub local: Option<bool>,
    /// Move a laptop run that passes its cap to the VM.
    pub vm_on_timeout: Option<bool>,
    /// The expected duration of every configuration not saying otherwise.
    pub expect: Option<Expect>,
    /// The time cap of every configuration not saying otherwise.
    pub timeout: Option<String>,
    /// The memory request of every configuration not saying otherwise.
    pub memory: Option<String>,
    /// `must_fail_only` for every configuration not saying otherwise.
    pub must_fail_only: Option<bool>,
    /// TLC options for every configuration not saying otherwise.
    pub tlc_opts: Option<Vec<String>>,
    /// The engine of every configuration not saying otherwise.
    pub engine: Option<Engine>,
}

/// A configuration to submit: its name, or a table with its own options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Cfg {
    /// Just the name.
    Name(String),
    /// The name and options of its own.
    Table(CfgTable),
}

/// A configuration given as a table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CfgTable {
    /// The configuration's file name.
    pub name: String,
    /// The expected duration.
    pub expect: Option<Expect>,
    /// A time cap replacing every lane cap, e.g. `"24h"`.
    pub timeout: Option<String>,
    /// The least memory of its VM attempts, e.g. `"64G"`.
    pub memory: Option<String>,
    /// Check only the property the header says must fail.
    pub must_fail_only: Option<bool>,
    /// Allowlisted TLC options.
    pub tlc_opts: Option<Vec<String>>,
    /// The engine.
    pub engine: Option<Engine>,
}

/// Options a configuration can set for itself, or the config for all of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CfgOptions {
    /// The expected duration.
    pub expect: Option<Expect>,
    /// A time cap replacing every lane cap, e.g. `"24h"`.
    pub timeout: Option<String>,
    /// The least memory of its VM attempts, e.g. `"64G"`.
    pub memory: Option<String>,
    /// Check only the property the header says must fail.
    pub must_fail_only: Option<bool>,
    /// Allowlisted TLC options.
    pub tlc_opts: Option<Vec<String>>,
    /// The engine.
    pub engine: Option<Engine>,
}

/// A submission with every layer applied and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one of submit's independent flags (D1)"
)]
pub struct Submission {
    /// The spec module.
    pub spec: String,
    /// Directories whose modules the spec reuses, relative to the model directory.
    pub extends: Vec<PathBuf>,
    /// The configurations named, each with its options; `None` means every one.
    pub cfgs: Option<Vec<Job>>,
    /// The options for configurations not named (when `cfgs` is `None`).
    pub defaults: JobOptions,
    /// Include max configurations when none is named.
    pub all: bool,
    /// Supersede this worktree's running older versions.
    pub replace: bool,
    /// Run every job on the laptop, uncapped.
    pub local: bool,
    /// Move a laptop run that passes its cap to the VM.
    pub vm_on_timeout: bool,
}

/// A named configuration with its effective options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The configuration's file name.
    pub name: String,
    /// Its options.
    pub options: JobOptions,
}

/// A configuration's effective, checked options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobOptions {
    /// The expected duration, if given.
    pub expect: Option<Expect>,
    /// The time cap, if given.
    pub timeout: Option<Duration>,
    /// The least memory of its VM attempts, in MB, if given.
    pub memory_mb: Option<u64>,
    /// Check only the property the header says must fail.
    pub must_fail_only: bool,
    /// Allowlisted TLC options, sorted.
    pub tlc_opts: Vec<String>,
    /// The engine (default tlx).
    pub engine: Engine,
}

/// Why a submission config was refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A config file could not be read.
    #[error("{path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A config is not valid TOML for this schema.
    #[error("{origin}: {source}")]
    Parse {
        /// Where it came from (a path, or `stdin`).
        origin: String,
        /// The underlying error.
        source: toml::de::Error,
    },
    /// No layer names the spec module.
    #[error("no spec module named: give `spec` in {FILE}, in --config, or on stdin")]
    NoSpec,
    /// The spec name is not a module name.
    #[error("spec {0:?} is not a module name (letters, digits and _)")]
    BadSpec(String),
    /// The spec module is not in the model directory.
    #[error("spec module {0}.tla is not in the model directory")]
    NoSpecModule(String),
    /// An `extends` entry is not a directory.
    #[error("extends {0}: not a directory")]
    NoExtendsDir(PathBuf),
    /// A timeout that is not a duration.
    #[error("timeout {0:?}: give a duration such as 90m, 24h or 1h30m")]
    BadTimeout(String),
    /// A memory request that is not a size, or under the least an attempt starts with.
    #[error("memory {0:?}: give a size of at least 4096M, such as 64G or 98304M")]
    BadMemory(String),
    /// A TLC option off the allowlist.
    #[error(transparent)]
    Option(#[from] options::NotAllowed),
}

/// Parse one layer from TOML text; `origin` names it in errors.
pub fn parse(text: &str, origin: &str) -> Result<Config, Error> {
    toml::from_str(text).map_err(|source| Error::Parse {
        origin: origin.to_owned(),
        source,
    })
}

/// Read `<dir>/tlcq.toml`, if there is one.
pub fn load_dir(dir: &Path) -> Result<Option<Config>, Error> {
    let path = dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text, &path.display().to_string()).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Read { path, source }),
    }
}

impl Config {
    /// This layer with `top` over it, key by key.
    pub fn under(self, top: Config) -> Config {
        Config {
            spec: top.spec.or(self.spec),
            extends: top.extends.or(self.extends),
            cfgs: under_cfgs(self.cfgs, top.cfgs),
            all: top.all.or(self.all),
            replace: top.replace.or(self.replace),
            local: top.local.or(self.local),
            vm_on_timeout: top.vm_on_timeout.or(self.vm_on_timeout),
            expect: top.expect.or(self.expect),
            timeout: top.timeout.or(self.timeout),
            memory: top.memory.or(self.memory),
            must_fail_only: top.must_fail_only.or(self.must_fail_only),
            tlc_opts: top.tlc_opts.or(self.tlc_opts),
            engine: top.engine.or(self.engine),
        }
    }

    /// The options this layer sets for every configuration.
    pub fn defaults(&self) -> CfgOptions {
        CfgOptions {
            expect: self.expect,
            timeout: self.timeout.clone(),
            memory: self.memory.clone(),
            must_fail_only: self.must_fail_only,
            tlc_opts: self.tlc_opts.clone(),
            engine: self.engine,
        }
    }

    /// Check the merged config against the model directory `dir`.
    pub fn resolve(self, dir: &Path) -> Result<Submission, Error> {
        let defaults = self.defaults();
        let spec = self.spec.ok_or(Error::NoSpec)?;
        if !is_module_name(&spec) {
            return Err(Error::BadSpec(spec));
        }
        if !has_file_named(dir, &format!("{spec}.tla")) {
            return Err(Error::NoSpecModule(spec));
        }
        let extends = self.extends.unwrap_or_default();
        if let Some(missing) = extends.iter().find(|e| !dir.join(e).is_dir()) {
            return Err(Error::NoExtendsDir(missing.clone()));
        }
        let cfgs = self
            .cfgs
            .map(|cfgs| {
                cfgs.into_iter()
                    .map(|cfg| cfg.resolve(&defaults))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        Ok(Submission {
            spec,
            extends,
            cfgs,
            defaults: defaults.resolve()?,
            all: self.all.unwrap_or(false),
            replace: self.replace.unwrap_or(false),
            local: self.local.unwrap_or(false),
            vm_on_timeout: self.vm_on_timeout.unwrap_or(false),
        })
    }
}

impl Submission {
    /// This submission with the command line's `flags` over every
    /// configuration's options, its own entry's included: a flag always wins
    /// (tlx-engine E1).
    ///
    /// # Errors
    /// If a flag's timeout is not a duration, its memory not a size, or an
    /// option is off the allowlist.
    pub fn with_flags(mut self, flags: &CfgOptions) -> Result<Self, Error> {
        let timeout = flags.timeout.as_deref().map(parse_duration).transpose()?;
        let memory_mb = flags.memory.as_deref().map(parse_memory).transpose()?;
        let tlc_opts = flags.tlc_opts.as_deref().map(options::check).transpose()?;
        let apply = |o: &mut JobOptions| {
            o.expect = flags.expect.or(o.expect);
            o.timeout = timeout.or(o.timeout);
            o.memory_mb = memory_mb.or(o.memory_mb);
            o.must_fail_only = flags.must_fail_only.unwrap_or(o.must_fail_only);
            if let Some(opts) = &tlc_opts {
                o.tlc_opts.clone_from(opts);
            }
            o.engine = flags.engine.unwrap_or(o.engine);
        };
        apply(&mut self.defaults);
        for job in self.cfgs.iter_mut().flatten() {
            apply(&mut job.options);
        }
        Ok(self)
    }
}

/// The `cfgs` of a layer `top` over those of `lower`: `top`'s when it has
/// them, each keeping the fields of `lower`'s entry of the same name that it
/// does not set itself; else `lower`'s.
fn under_cfgs(lower: Option<Vec<Cfg>>, top: Option<Vec<Cfg>>) -> Option<Vec<Cfg>> {
    let Some(top) = top else {
        return lower;
    };
    let lower = lower.unwrap_or_default();
    let merged = top
        .into_iter()
        .map(|cfg| match lower.iter().find(|l| l.name() == cfg.name()) {
            Some(l) => Cfg::Table(l.clone().table().under(cfg.table())),
            None => cfg,
        })
        .collect();
    Some(merged)
}

impl Cfg {
    /// The configuration's file name.
    fn name(&self) -> &str {
        match self {
            Cfg::Name(name) => name,
            Cfg::Table(t) => &t.name,
        }
    }

    /// The configuration as a table: a bare name sets no field.
    fn table(self) -> CfgTable {
        match self {
            Cfg::Name(name) => CfgTable::of(name, CfgOptions::default()),
            Cfg::Table(t) => t,
        }
    }

    fn resolve(self, defaults: &CfgOptions) -> Result<Job, Error> {
        let t = self.table();
        let own = CfgOptions {
            expect: t.expect,
            timeout: t.timeout,
            memory: t.memory,
            must_fail_only: t.must_fail_only,
            tlc_opts: t.tlc_opts,
            engine: t.engine,
        };
        Ok(Job {
            name: t.name,
            options: defaults.clone().under(own).resolve()?,
        })
    }
}

impl CfgTable {
    /// The entry `name` with the fields `options`.
    fn of(name: String, options: CfgOptions) -> CfgTable {
        CfgTable {
            name,
            expect: options.expect,
            timeout: options.timeout,
            memory: options.memory,
            must_fail_only: options.must_fail_only,
            tlc_opts: options.tlc_opts,
            engine: options.engine,
        }
    }

    /// This entry with `top`'s fields over it, field by field; `top`'s name.
    fn under(self, top: CfgTable) -> CfgTable {
        CfgTable {
            name: top.name,
            expect: top.expect.or(self.expect),
            timeout: top.timeout.or(self.timeout),
            memory: top.memory.or(self.memory),
            must_fail_only: top.must_fail_only.or(self.must_fail_only),
            tlc_opts: top.tlc_opts.or(self.tlc_opts),
            engine: top.engine.or(self.engine),
        }
    }
}

impl CfgOptions {
    /// These options with `top` over them, key by key.
    pub fn under(self, top: CfgOptions) -> CfgOptions {
        CfgOptions {
            expect: top.expect.or(self.expect),
            timeout: top.timeout.or(self.timeout),
            memory: top.memory.or(self.memory),
            must_fail_only: top.must_fail_only.or(self.must_fail_only),
            tlc_opts: top.tlc_opts.or(self.tlc_opts),
            engine: top.engine.or(self.engine),
        }
    }

    fn resolve(self) -> Result<JobOptions, Error> {
        Ok(JobOptions {
            expect: self.expect,
            timeout: self.timeout.map(|t| parse_duration(&t)).transpose()?,
            memory_mb: self.memory.as_deref().map(parse_memory).transpose()?,
            must_fail_only: self.must_fail_only.unwrap_or(false),
            tlc_opts: options::check(&self.tlc_opts.unwrap_or_default())?,
            engine: self.engine.unwrap_or_default(),
        })
    }
}

/// Whether `dir` holds a regular file named exactly `name`, case included: macOS
/// would open `juliet.tla` as `Juliet.tla`, which TLC on the VM would not.
fn has_file_named(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.file_name() == name && e.path().is_file())
    })
}

/// Whether `name` can be a TLA+ module name: letters, digits and `_`.
pub fn is_module_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse a duration such as `90m`, `24h` or `1h30m` (units `h`, `m`, `s`).
pub fn parse_duration(text: &str) -> Result<Duration, Error> {
    let bad = || Error::BadTimeout(text.to_owned());
    let mut secs: u64 = 0;
    let mut number: Option<u64> = None;
    for c in text.chars() {
        if let Some(d) = c.to_digit(10) {
            number = Some(
                number
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|n| n.checked_add(u64::from(d)))
                    .ok_or_else(bad)?,
            );
            continue;
        }
        let unit = match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return Err(bad()),
        };
        let n = number.take().ok_or_else(bad)?;
        secs = n
            .checked_mul(unit)
            .and_then(|s| secs.checked_add(s))
            .ok_or_else(bad)?;
    }
    if number.is_some() || secs == 0 {
        return Err(bad());
    }
    Ok(Duration::from_secs(secs))
}

/// Parse a memory request such as `64G` or `98304M` into MB: an integer with
/// an optional `M`, `G` or `T` suffix (powers of 1024; none is MB), at least
/// what an attempt starts with (job-memory M7).
pub fn parse_memory(text: &str) -> Result<u64, Error> {
    let bad = || Error::BadMemory(text.to_owned());
    let (digits, scale) = match text.strip_suffix(['M', 'G', 'T']) {
        Some(d) if text.ends_with('G') => (d, 1 << 10),
        Some(d) if text.ends_with('T') => (d, 1 << 20),
        Some(d) => (d, 1),
        None => (text, 1),
    };
    let n: u64 = digits.parse().ok().ok_or_else(bad)?;
    n.checked_mul(scale)
        .filter(|&mb| mb >= crate::runner::memory::MIN_MB)
        .ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JULIET: &str = r#"
spec = "Juliet"
extends = ["../november"]
replace = true
cfgs = [
  "JA.cfg",
  { name = "JA_10.cfg", must_fail_only = true },
  { name = "JA_08_max.cfg", timeout = "24h" },
]
"#;

    /// A model directory `juliet` holding `Juliet.tla`, beside `november/`.
    fn model() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("juliet");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(root.path().join("november")).unwrap();
        std::fs::write(dir.join("Juliet.tla"), "").unwrap();
        (root, dir)
    }

    fn job(name: &str, options: JobOptions) -> Job {
        Job {
            name: name.to_owned(),
            options,
        }
    }

    #[test]
    fn resolves_a_real_submission() {
        let (_root, dir) = model();
        let s = parse(JULIET, "stdin").unwrap().resolve(&dir).unwrap();
        assert_eq!(s.spec, "Juliet");
        assert_eq!(s.extends, [PathBuf::from("../november")]);
        assert!(s.replace);
        assert_eq!(
            s.cfgs.unwrap(),
            [
                job("JA.cfg", JobOptions::default()),
                job(
                    "JA_10.cfg",
                    JobOptions {
                        must_fail_only: true,
                        ..JobOptions::default()
                    }
                ),
                job(
                    "JA_08_max.cfg",
                    JobOptions {
                        timeout: Some(Duration::from_secs(24 * 3600)),
                        ..JobOptions::default()
                    }
                ),
            ]
        );
    }

    #[test]
    fn layers_override_key_by_key() {
        let (_root, dir) = model();
        std::fs::write(
            dir.join(FILE),
            "spec = \"Juliet\"\nextends = [\"../november\"]\nexpect = \"slow\"\n",
        )
        .unwrap();
        let base = load_dir(&dir).unwrap().unwrap();
        let stdin = parse(
            "cfgs = [\"JA.cfg\", { name = \"JA_08_max.cfg\", expect = \"max\" }]\nreplace = true\n",
            "stdin",
        )
        .unwrap();
        let line = Config {
            replace: Some(false),
            ..Config::default()
        };
        let s = base.under(stdin).under(line).resolve(&dir).unwrap();
        assert_eq!(s.extends, [PathBuf::from("../november")]);
        assert!(!s.replace);
        let cfgs = s.cfgs.unwrap();
        let expects: Vec<_> = cfgs.iter().map(|j| j.options.expect).collect();
        assert_eq!(expects, [Some(Expect::Slow), Some(Expect::Max)]);
        assert_eq!(s.defaults.expect, Some(Expect::Slow));
    }

    #[test]
    fn no_file_in_the_dir_is_no_layer() {
        let (_root, dir) = model();
        assert_eq!(load_dir(&dir).unwrap(), None);
    }

    #[test]
    fn an_unreadable_file_is_a_read_error() {
        let (_root, dir) = model();
        std::fs::create_dir(dir.join(FILE)).unwrap();
        assert!(matches!(load_dir(&dir), Err(Error::Read { .. })));
    }

    #[test]
    fn unknown_keys_are_refused() {
        let err = parse("spec = \"A\"\nspecs = 1\n", "stdin").unwrap_err();
        assert!(err.to_string().starts_with("stdin: "), "{err}");
        parse("cfgs = [{ name = \"a.cfg\", bogus = 1 }]\n", "stdin").unwrap_err();
        parse("expect = \"huge\"\n", "stdin").unwrap_err();
    }

    #[test]
    fn the_spec_must_be_named_and_present() {
        let (_root, dir) = model();
        assert!(matches!(
            Config::default().resolve(&dir),
            Err(Error::NoSpec)
        ));
        let named = |s: &str| Config {
            spec: Some(s.to_owned()),
            ..Config::default()
        };
        assert!(matches!(
            named("Juliet.tla").resolve(&dir),
            Err(Error::BadSpec(_))
        ));
        assert!(matches!(
            named("Other").resolve(&dir),
            Err(Error::NoSpecModule(_))
        ));
        // The case must match too, even on a case-insensitive disk.
        assert!(matches!(
            named("juliet").resolve(&dir),
            Err(Error::NoSpecModule(_))
        ));
        assert!(matches!(
            named("Juliet").resolve(&dir.join("nope")),
            Err(Error::NoSpecModule(_))
        ));
    }

    #[test]
    fn extends_must_be_directories() {
        let (_root, dir) = model();
        let c = parse("spec = \"Juliet\"\nextends = [\"../nope\"]\n", "x").unwrap();
        assert!(matches!(c.resolve(&dir), Err(Error::NoExtendsDir(_))));
    }

    #[test]
    fn options_are_checked() {
        let (_root, dir) = model();
        let c = parse("spec = \"Juliet\"\ntlc_opts = [\"-continue\"]\n", "x").unwrap();
        assert!(matches!(c.resolve(&dir), Err(Error::Option(_))));
        let c = parse(
            "spec = \"Juliet\"\ncfgs = [{ name = \"a.cfg\", timeout = \"soon\" }]\n",
            "x",
        )
        .unwrap();
        assert!(matches!(c.resolve(&dir), Err(Error::BadTimeout(_))));
    }

    #[test]
    fn the_engine_is_tlx_unless_a_layer_or_an_entry_says_otherwise() {
        let (_root, dir) = model();
        let s = parse("spec = \"Juliet\"\ncfgs = [\"A.cfg\"]\n", "x")
            .unwrap()
            .resolve(&dir)
            .unwrap();
        assert_eq!(s.defaults.engine, Engine::Tlx);
        assert_eq!(s.cfgs.unwrap()[0].options.engine, Engine::Tlx);
        let s = parse(
            "spec = \"Juliet\"\nengine = \"tlc\"\ncfgs = [\"A.cfg\", { name = \"B.cfg\", engine = \"tlx\" }]\n",
            "x",
        )
        .unwrap()
        .resolve(&dir)
        .unwrap();
        let engines: Vec<_> = s.cfgs.unwrap().iter().map(|j| j.options.engine).collect();
        assert_eq!(engines, [Engine::Tlc, Engine::Tlx]);
        parse("engine = \"java\"\n", "x").unwrap_err();
    }

    #[test]
    fn a_flag_wins_over_every_entry_named_or_not() {
        let (_root, dir) = model();
        std::fs::write(
            dir.join(FILE),
            "spec = \"Juliet\"\ncfgs = [{ name = \"NB.cfg\", expect = \"fast\", timeout = \"24h\", engine = \"tlc\", must_fail_only = true }]\n",
        )
        .unwrap();
        let flags = CfgOptions {
            expect: Some(Expect::Max),
            timeout: Some("10m".to_owned()),
            tlc_opts: Some(vec!["-deadlock".to_owned()]),
            engine: Some(Engine::Tlx),
            ..CfgOptions::default()
        };
        // Named on the command line, or one of the directory's `cfgs`.
        let named = Config {
            cfgs: Some(vec![Cfg::Name("NB.cfg".to_owned())]),
            ..Config::default()
        };
        for line in [named, Config::default()] {
            let s = load_dir(&dir)
                .unwrap()
                .unwrap()
                .under(line)
                .resolve(&dir)
                .unwrap()
                .with_flags(&flags)
                .unwrap();
            let o = &s.cfgs.unwrap()[0].options;
            assert_eq!(o.expect, Some(Expect::Max));
            assert_eq!(o.timeout, Some(Duration::from_secs(600)));
            assert_eq!(o.tlc_opts, ["-deadlock"]);
            assert_eq!(o.engine, Engine::Tlx);
            // What the flags leave alone, the entry still gives.
            assert!(o.must_fail_only);
            assert_eq!(s.defaults.engine, Engine::Tlx);
        }
    }

    #[test]
    fn a_flag_that_is_not_valid_is_refused() {
        let (_root, dir) = model();
        let s = || {
            parse("spec = \"Juliet\"\n", "x")
                .unwrap()
                .resolve(&dir)
                .unwrap()
        };
        let bad_timeout = CfgOptions {
            timeout: Some("soon".to_owned()),
            ..CfgOptions::default()
        };
        assert!(matches!(
            s().with_flags(&bad_timeout),
            Err(Error::BadTimeout(_))
        ));
        let bad_option = CfgOptions {
            tlc_opts: Some(vec!["-continue".to_owned()]),
            ..CfgOptions::default()
        };
        assert!(matches!(s().with_flags(&bad_option), Err(Error::Option(_))));
    }

    #[test]
    fn a_configuration_named_on_the_command_line_keeps_its_entry() {
        let (_root, dir) = model();
        std::fs::write(
            dir.join(FILE),
            "spec = \"Juliet\"\ncfgs = [{ name = \"NB_08_live.cfg\", engine = \"tlc\", timeout = \"24h\" }, \"JA.cfg\"]\n",
        )
        .unwrap();
        let base = load_dir(&dir).unwrap().unwrap();
        let line = Config {
            cfgs: Some(vec![
                Cfg::Name("NB_08_live.cfg".to_owned()),
                Cfg::Name("Other.cfg".to_owned()),
            ]),
            ..Config::default()
        };
        let cfgs = base.under(line).resolve(&dir).unwrap().cfgs.unwrap();
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].name, "NB_08_live.cfg");
        assert_eq!(cfgs[0].options.engine, Engine::Tlc);
        assert_eq!(
            cfgs[0].options.timeout,
            Some(Duration::from_secs(24 * 3600))
        );
        assert_eq!(cfgs[1].name, "Other.cfg");
        assert_eq!(cfgs[1].options, JobOptions::default());
        // A field the upper entry sets wins over the lower entry's.
        let upper = parse(
            "cfgs = [{ name = \"NB_08_live.cfg\", engine = \"tlx\" }]\n",
            "stdin",
        )
        .unwrap();
        let cfgs = load_dir(&dir)
            .unwrap()
            .unwrap()
            .under(upper)
            .resolve(&dir)
            .unwrap()
            .cfgs
            .unwrap();
        assert_eq!(cfgs[0].options.engine, Engine::Tlx);
        assert_eq!(
            cfgs[0].options.timeout,
            Some(Duration::from_secs(24 * 3600))
        );
    }

    #[test]
    fn an_entry_beats_a_file_s_top_level_keys_and_a_flag_beats_both() {
        let (_root, dir) = model();
        std::fs::write(
            dir.join(FILE),
            "spec = \"Juliet\"\ncfgs = [{ name = \"NB.cfg\", engine = \"tlc\", timeout = \"24h\" }]\n",
        )
        .unwrap();
        // `--config` names it and sets a timeout for everything: its entry's
        // own 24h wins, as entries beat top-level keys.
        let config = parse("timeout = \"1h\"\ncfgs = [\"NB.cfg\"]\n", "sub.toml").unwrap();
        let layered = || load_dir(&dir).unwrap().unwrap().under(config.clone());
        let cfgs = layered().resolve(&dir).unwrap().cfgs.unwrap();
        assert_eq!(
            cfgs[0].options.timeout,
            Some(Duration::from_secs(24 * 3600))
        );
        // The command line's own timeout wins over it.
        let line = Config {
            timeout: Some("10m".to_owned()),
            ..Config::default()
        };
        let overrides = line.defaults();
        let cfgs = layered()
            .under(line)
            .resolve(&dir)
            .unwrap()
            .with_flags(&overrides)
            .unwrap()
            .cfgs
            .unwrap();
        assert_eq!(cfgs[0].options.timeout, Some(Duration::from_secs(600)));
        assert_eq!(cfgs[0].options.engine, Engine::Tlc);
    }

    #[test]
    fn a_memory_request_layers_as_the_other_fields_and_the_flag_wins() {
        let (_root, dir) = model();
        std::fs::write(
            dir.join(FILE),
            "spec = \"Juliet\"\nmemory = \"8G\"\ncfgs = [{ name = \"RP_live_max.cfg\", memory = \"96G\" }, \"JA.cfg\"]\n",
        )
        .unwrap();
        let file = || load_dir(&dir).unwrap().unwrap();
        let cfgs = file().resolve(&dir).unwrap().cfgs.unwrap();
        // The entry's own request beats the file's top-level one.
        assert_eq!(cfgs[0].options.memory_mb, Some(96 * 1024));
        assert_eq!(cfgs[1].options.memory_mb, Some(8 * 1024));
        // `--memory` beats both.
        let line = Config {
            cfgs: Some(vec![Cfg::Name("RP_live_max.cfg".to_owned())]),
            memory: Some("64G".to_owned()),
            ..Config::default()
        };
        let overrides = line.defaults();
        let cfgs = file()
            .under(line)
            .resolve(&dir)
            .unwrap()
            .with_flags(&overrides)
            .unwrap()
            .cfgs
            .unwrap();
        assert_eq!(cfgs[0].options.memory_mb, Some(64 * 1024));
        // A request that is not a size, in the file or as the flag.
        std::fs::write(
            dir.join(FILE),
            "spec = \"Juliet\"\ncfgs = [{ name = \"JA.cfg\", memory = \"lots\" }]\n",
        )
        .unwrap();
        assert!(matches!(
            file().resolve(&dir),
            Err(Error::BadMemory(m)) if m == "lots"
        ));
        let bad = CfgOptions {
            memory: Some("2G".to_owned()),
            ..CfgOptions::default()
        };
        let plain = parse("spec = \"Juliet\"\n", "stdin").unwrap();
        assert!(matches!(
            plain.resolve(&dir).unwrap().with_flags(&bad),
            Err(Error::BadMemory(_))
        ));
    }

    #[test]
    fn memory_sizes() {
        assert_eq!(parse_memory("64G").unwrap(), 65_536);
        assert_eq!(parse_memory("98304M").unwrap(), 98_304);
        assert_eq!(parse_memory("98304").unwrap(), 98_304);
        assert_eq!(parse_memory("1T").unwrap(), 1_048_576);
        assert_eq!(parse_memory("4096").unwrap(), 4096);
        for bad in [
            "",
            "G",
            "64g",
            "64GB",
            "4095M",
            "3G",
            "-1G",
            "18446744073709551615T",
        ] {
            parse_memory(bad).unwrap_err();
        }
    }

    #[test]
    fn module_names() {
        assert!(is_module_name("Juliet_2"));
        assert!(!is_module_name(""));
        assert!(!is_module_name("a-b"));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        for bad in [
            "",
            "24",
            "h",
            "0h",
            "1d",
            "99999999999999999999h",
            "18446744073709551619h",
            "1h2",
        ] {
            parse_duration(bad).unwrap_err();
        }
    }
}
