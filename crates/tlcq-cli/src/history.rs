//! Expected duration and the laptop's history (design D5).
//!
//! Each job is expected short (under 5 minutes), long, max (over 1 hour), or
//! unknown: by a flag (or the config's `expect`), else by a configuration name
//! suffix (`_fast`, `_slow`, `_max`), else by the history, first match wins:
//!
//! | Lookup        | Key                                   | Use                                  |
//! |---------------|---------------------------------------|--------------------------------------|
//! | exact         | spec hash + cfg hash + options hash   | the key's expectation                |
//! | configuration | spec module + cfg hash + options hash | the same, shown "from an earlier spec" |
//! | name          | spec module + configuration name      | only when it is long or max          |
//!
//! The history is an append-only JSONL file, one record per job id and end,
//! written under a lock. A key's expectation is its last finished record raised
//! by any later lower bound, never lowered by one.
//!
//! Each lookup sees only the records of the job's engine; a tlx job with none
//! falls back to TLC's, as an upper bound (tlx-engine E7). A record from
//! before engines reads as TLC's.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Expect;
use crate::engine::Engine;
use crate::id::sha256;
use crate::job::{Contents, CFG};
use crate::outcome::LaneDecision;

/// Under this, a finished run is short; at or past it, a lower bound is long.
pub const SHORT: Duration = Duration::from_secs(5 * 60);
/// Past this, a finished run is max; at or past it, a lower bound is max.
pub const MAX: Duration = Duration::from_secs(60 * 60);

/// An expected duration, ordered short < long < max.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Expected {
    /// Under 5 minutes.
    Short,
    /// Between 5 minutes and an hour.
    Long,
    /// Over an hour.
    Max,
}

impl From<Expect> for Expected {
    fn from(e: Expect) -> Self {
        match e {
            Expect::Fast => Expected::Short,
            Expect::Slow => Expected::Long,
            Expect::Max => Expected::Max,
        }
    }
}

/// Where an expectation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A flag or the config's `expect`.
    Flag,
    /// The configuration name's suffix.
    Suffix,
    /// The history, under the exact key.
    Exact,
    /// The history, under the configuration key: from an earlier spec.
    Configuration,
    /// The history, under the name key.
    Name,
    /// TLC's history, for a tlx job with none of its own: an upper bound,
    /// tlx being meant to be faster (tlx-engine E7).
    Tlc,
}

/// The keys a job's history is looked up under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keys {
    /// The engine the job runs on; TLC for a record from before tlx.
    #[serde(default = "Engine::stored")]
    pub engine: Engine,
    /// The SHA-256 of the manifest without `model/job.cfg`.
    pub spec_hash: String,
    /// The SHA-256 of the configuration's bytes.
    pub cfg_hash: String,
    /// The SHA-256 of the options.
    pub options_hash: String,
    /// The spec module's name.
    pub spec: String,
    /// The configuration's file name.
    pub cfg_name: String,
}

impl Keys {
    /// The keys of a job on `engine`, from its manifest (`id::manifest`) and
    /// contents.
    pub fn new(
        engine: Engine,
        manifest: &str,
        contents: &Contents,
        options: &[String],
        spec: &str,
        cfg_name: &str,
    ) -> Self {
        let cfg_line = format!("file {CFG} ");
        let without_cfg: String = manifest
            .split_inclusive('\n')
            .filter(|line| !line.starts_with(&cfg_line))
            .collect();
        let cfg = contents
            .files()
            .iter()
            .find(|f| f.path == CFG)
            .map_or(&[][..], |f| f.bytes.as_slice());
        Self {
            engine,
            spec_hash: sha256(without_cfg.as_bytes()),
            cfg_hash: sha256(cfg),
            options_hash: sha256(options.join("\n").as_bytes()),
            spec: spec.to_owned(),
            cfg_name: cfg_name.to_owned(),
        }
    }

    /// Whether `other` is this configuration under D5's exact or
    /// configuration key: the same configuration and options, of this spec,
    /// on the same engine (estimates never cross engines, E7).
    pub fn same_configuration(&self, other: &Keys) -> bool {
        self.engine == other.engine
            && self.cfg_hash == other.cfg_hash
            && self.options_hash == other.options_hash
            && (self.spec_hash == other.spec_hash || self.spec == other.spec)
    }

    /// Whether `other` has this spec module and configuration name (D5's name
    /// key), on the same engine.
    pub fn same_name(&self, other: &Keys) -> bool {
        self.engine == other.engine && self.spec == other.spec && self.cfg_name == other.cfg_name
    }
}

/// One run's end, as the history keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The job's id.
    pub id: String,
    /// The run that ended (`l<n>` or `v<n>`): with the id, what makes a record unique.
    pub end: String,
    /// The keys it is looked up under, one per configuration name of the job
    /// (several for a merged job).
    pub names: Vec<Keys>,
    /// What the run implies: `None` for a run that ended early under 5 minutes.
    pub expected: Option<Expected>,
    /// Whether `expected` is only a lower bound (the run ended before finishing).
    pub lower_bound: bool,
    /// The run's duration, in seconds of awake time.
    pub duration_secs: u64,
    /// The run's outcome.
    pub outcome: String,
    /// Where it ran: `laptop` or `vm`.
    pub place: String,
    /// Its progress curves, one per attempt.
    pub curves: Vec<String>,
    /// The VM's lane decisions, with the probe's numbers, for later tuning.
    #[serde(default)]
    pub lanes: Vec<LaneDecision>,
}

/// What a run's end implies: a finished run its duration's lane; one that ended
/// early a lower bound (long at or past 5 minutes, max at or past 1 hour, nothing
/// below); a run the VM judged max, max whatever its duration.
pub fn implied(duration: Duration, finished: bool, judged_max: bool) -> Option<Expected> {
    if judged_max {
        Some(Expected::Max)
    } else if finished {
        Some(if duration < SHORT {
            Expected::Short
        } else if duration <= MAX {
            Expected::Long
        } else {
            Expected::Max
        })
    } else if duration >= MAX {
        Some(Expected::Max)
    } else if duration >= SHORT {
        Some(Expected::Long)
    } else {
        None
    }
}

/// The expectation a configuration name's suffix gives, if any.
pub fn suffix(cfg_name: &str) -> Option<Expected> {
    let stem = cfg_name.strip_suffix(".cfg").unwrap_or(cfg_name);
    [
        ("_fast", Expected::Short),
        ("_slow", Expected::Long),
        ("_max", Expected::Max),
    ]
    .into_iter()
    .find_map(|(s, e)| stem.ends_with(s).then_some(e))
}

/// The laptop's history.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    records: Vec<Record>,
}

/// Why the history could not be read or written.
#[derive(Debug, thiserror::Error)]
#[error("history {path}: {source}")]
pub struct Error {
    path: String,
    source: std::io::Error,
}

impl History {
    /// Read the history at `path` under a shared lock. A missing file is an empty
    /// history; a line that does not parse (a write cut off by a crash) is skipped.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let err = |source| Error {
            path: path.display().to_string(),
            source,
        };
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(err(e)),
        };
        file.lock_shared().map_err(err)?;
        let records = parse_lines(&read_all(&file).map_err(err)?);
        Ok(Self { records })
    }

    /// Append `record` under an exclusive lock, unless a record of the same id and
    /// end is already there. Returns whether it was written.
    pub fn append(path: &Path, record: &Record) -> Result<bool, Error> {
        let err = |source| Error {
            path: path.display().to_string(),
            source,
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)
            .map_err(err)?;
        file.lock().map_err(err)?;
        let bytes = read_all(&file).map_err(err)?;
        let seen = parse_lines(&bytes)
            .iter()
            .any(|r| r.id == record.id && r.end == record.end);
        if seen {
            return Ok(false);
        }
        let mut line = serde_json::to_string(record).map_err(|e| err(e.into()))?;
        line.push('\n');
        if bytes.last().is_some_and(|b| *b != b'\n') {
            // A line cut off by a crash: end it, so this record starts a line of its own.
            line.insert(0, '\n');
        }
        file.write_all(line.as_bytes()).map_err(err)?;
        Ok(true)
    }

    /// The records, oldest first.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The expectation of the records `matches` selects: the last finished one,
    /// raised by any later lower bound.
    fn key(&self, matches: impl Fn(&Keys) -> bool) -> Option<Expected> {
        self.records
            .iter()
            .filter(|r| r.names.iter().any(&matches))
            .fold(None, |acc, r| match (r.lower_bound, r.expected) {
                (_, None) => acc,
                (false, Some(e)) => Some(e),
                (true, Some(e)) => Some(acc.map_or(e, |a| a.max(e))),
            })
    }

    /// A job's expectation from the history alone: exact, then configuration,
    /// then name (used only when long or max), over the records of the job's
    /// engine; for a tlx job with none, the same over TLC's records (E7).
    pub fn lookup(&self, keys: &Keys) -> Option<(Expected, Source)> {
        self.lookup_on(keys, keys.engine).or_else(|| {
            (keys.engine != Engine::Tlc)
                .then(|| self.lookup_on(keys, Engine::Tlc))
                .flatten()
                .map(|(e, _)| (e, Source::Tlc))
        })
    }

    /// [`History::lookup`] over the records of `engine` only.
    fn lookup_on(&self, keys: &Keys, engine: Engine) -> Option<(Expected, Source)> {
        let exact = |k: &Keys| {
            k.engine == engine
                && k.spec_hash == keys.spec_hash
                && k.cfg_hash == keys.cfg_hash
                && k.options_hash == keys.options_hash
        };
        let configuration = |k: &Keys| {
            k.engine == engine
                && k.spec == keys.spec
                && k.cfg_hash == keys.cfg_hash
                && k.options_hash == keys.options_hash
        };
        let name =
            |k: &Keys| k.engine == engine && k.spec == keys.spec && k.cfg_name == keys.cfg_name;
        self.key(exact)
            .map(|e| (e, Source::Exact))
            .or_else(|| self.key(configuration).map(|e| (e, Source::Configuration)))
            .or_else(|| {
                self.key(name)
                    .filter(|e| *e >= Expected::Long)
                    .map(|e| (e, Source::Name))
            })
    }
}

/// The whole file, from the start.
fn read_all(mut file: &std::fs::File) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Every line that parses as a record; a line that does not (cut off by a crash,
/// or not even UTF-8) is skipped.
fn parse_lines(bytes: &[u8]) -> Vec<Record> {
    bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// A job's expectation, first match wins: the flag, the suffix, the history;
/// `None` is unknown. Also a warning when the history contradicts the suffix.
pub fn expectation(
    flag: Option<Expect>,
    keys: &Keys,
    history: &History,
) -> (Option<(Expected, Source)>, Option<String>) {
    let by_suffix = suffix(&keys.cfg_name);
    let by_history = history.lookup(keys);
    let warning = match (by_suffix, by_history) {
        (Some(s), Some((h, _))) if s != h => Some(format!(
            "{}: its suffix says {s:?}, its history {h:?}; the suffix wins (rename it to change that)",
            keys.cfg_name
        )),
        _ => None,
    };
    let chosen = flag
        .map(|f| (f.into(), Source::Flag))
        .or(by_suffix.map(|s| (s, Source::Suffix)))
        .or(by_history);
    (chosen, warning)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{File, Limits};

    fn keys(spec_hash: &str, cfg_hash: &str, cfg_name: &str) -> Keys {
        Keys {
            engine: crate::engine::Engine::Tlc,
            spec_hash: spec_hash.to_owned(),
            cfg_hash: cfg_hash.to_owned(),
            options_hash: "o".to_owned(),
            spec: "Juliet".to_owned(),
            cfg_name: cfg_name.to_owned(),
        }
    }

    fn record(end: &str, keys: Keys, expected: Option<Expected>, lower_bound: bool) -> Record {
        Record {
            id: "id".to_owned(),
            end: end.to_owned(),
            names: vec![keys],
            expected,
            lower_bound,
            duration_secs: 0,
            outcome: "ok".to_owned(),
            place: "vm".to_owned(),
            curves: Vec::new(),
            lanes: Vec::new(),
        }
    }

    fn history(records: Vec<Record>) -> History {
        History { records }
    }

    const MIN: u64 = 60;

    fn on_tlx(k: Keys) -> Keys {
        Keys {
            engine: Engine::Tlx,
            ..k
        }
    }

    #[test]
    fn a_tlx_job_s_own_history_comes_first_then_tlc_s_as_a_bound() {
        let tlc = keys("s-tlc", "c", "A.cfg");
        let job = on_tlx(keys("s-tlx", "c", "A.cfg"));
        // No tlx record: TLC's, under the configuration key, as from TLC.
        let h = history(vec![record(
            "l1",
            tlc.clone(),
            Some(Expected::Short),
            false,
        )]);
        assert_eq!(h.lookup(&job), Some((Expected::Short, Source::Tlc)));
        // A tlx record wins over TLC's.
        let h = history(vec![
            record("v1", tlc.clone(), Some(Expected::Max), false),
            record("v2", job.clone(), Some(Expected::Short), false),
        ]);
        assert_eq!(h.lookup(&job), Some((Expected::Short, Source::Exact)));
        // A TLC job never takes tlx's.
        let h = history(vec![record(
            "v1",
            job.clone(),
            Some(Expected::Short),
            false,
        )]);
        assert_eq!(h.lookup(&tlc), None);
        // Nor does an estimate's previous run cross engines.
        assert!(!job.same_configuration(&tlc) && !job.same_name(&tlc));
        assert!(job.same_name(&job));
    }

    #[test]
    fn a_record_from_before_engines_reads_as_tlc_s() {
        let mut json =
            serde_json::to_value(record("v1", keys("s", "c", "A.cfg"), None, false)).unwrap();
        json["names"][0].as_object_mut().unwrap().remove("engine");
        let r: Record = serde_json::from_value(json).unwrap();
        assert_eq!(r.names[0].engine, Engine::Tlc);
    }

    #[test]
    fn implied_lanes() {
        let d = |m| Duration::from_secs(m * MIN);
        assert_eq!(implied(d(4), true, false), Some(Expected::Short));
        assert_eq!(implied(d(5), true, false), Some(Expected::Long));
        assert_eq!(implied(d(60), true, false), Some(Expected::Long));
        assert_eq!(implied(d(61), true, false), Some(Expected::Max));
        assert_eq!(implied(d(4), false, false), None);
        assert_eq!(implied(d(5), false, false), Some(Expected::Long));
        assert_eq!(implied(d(60), false, false), Some(Expected::Max));
        assert_eq!(implied(d(1), true, true), Some(Expected::Max));
    }

    #[test]
    fn flags_map_to_lanes() {
        assert_eq!(Expected::from(Expect::Fast), Expected::Short);
        assert_eq!(Expected::from(Expect::Slow), Expected::Long);
        assert_eq!(Expected::from(Expect::Max), Expected::Max);
    }

    #[test]
    fn suffixes() {
        assert_eq!(suffix("JA_04_live_max.cfg"), Some(Expected::Max));
        assert_eq!(suffix("A_fast.cfg"), Some(Expected::Short));
        assert_eq!(suffix("A_slow"), Some(Expected::Long));
        assert_eq!(suffix("JA_09_maxed.cfg"), None);
    }

    #[test]
    fn a_short_cancel_never_lowers_a_long_run() {
        let k = keys("s", "c", "JA.cfg");
        let h = history(vec![
            record("v1", k.clone(), Some(Expected::Max), false),
            record("v2", k.clone(), Some(Expected::Long), true),
        ]);
        assert_eq!(h.lookup(&k), Some((Expected::Max, Source::Exact)));
    }

    #[test]
    fn a_lower_bound_raises_and_a_finished_run_replaces() {
        let k = keys("s", "c", "JA.cfg");
        let raised = history(vec![
            record("l1", k.clone(), Some(Expected::Short), false),
            record("l2", k.clone(), Some(Expected::Long), true),
            record("l3", k.clone(), None, true),
        ]);
        assert_eq!(raised.lookup(&k), Some((Expected::Long, Source::Exact)));
        let replaced = history(vec![
            record("v1", k.clone(), Some(Expected::Max), false),
            record("v2", k.clone(), Some(Expected::Short), false),
        ]);
        assert_eq!(replaced.lookup(&k), Some((Expected::Short, Source::Exact)));
    }

    #[test]
    fn a_spec_edit_keeps_the_configuration_key() {
        let old = keys("s1", "c", "JA.cfg");
        let h = history(vec![record("v1", old, Some(Expected::Max), false)]);
        assert_eq!(
            h.lookup(&keys("s2", "c", "JA.cfg")),
            Some((Expected::Max, Source::Configuration))
        );
    }

    #[test]
    fn the_name_key_only_lengthens() {
        let h = history(vec![record(
            "v1",
            keys("s1", "c1", "JA.cfg"),
            Some(Expected::Long),
            false,
        )]);
        assert_eq!(
            h.lookup(&keys("s2", "c2", "JA.cfg")),
            Some((Expected::Long, Source::Name))
        );
        let short = history(vec![record(
            "v1",
            keys("s1", "c1", "JA.cfg"),
            Some(Expected::Short),
            false,
        )]);
        assert_eq!(short.lookup(&keys("s2", "c2", "JA.cfg")), None);
    }

    #[test]
    fn first_match_wins_and_a_contradicted_suffix_warns() {
        let k = keys("s", "c", "JA_03_fast.cfg");
        let h = history(vec![record("v1", k.clone(), Some(Expected::Long), false)]);
        let (chosen, warning) = expectation(None, &k, &h);
        assert_eq!(chosen, Some((Expected::Short, Source::Suffix)));
        assert!(warning.unwrap().contains("JA_03_fast.cfg"));
        let (chosen, _) = expectation(Some(Expect::Max), &k, &h);
        assert_eq!(chosen, Some((Expected::Max, Source::Flag)));
        let plain = keys("s", "c", "JA.cfg");
        assert_eq!(
            expectation(None, &plain, &h),
            (Some((Expected::Long, Source::Exact)), None)
        );
        assert_eq!(expectation(None, &plain, &History::default()), (None, None));
    }

    #[test]
    fn keys_split_the_spec_from_the_configuration() {
        let contents = |cfg: &str, module: &str| {
            Contents::new(
                vec![
                    File {
                        path: "model/M.tla".to_owned(),
                        bytes: module.as_bytes().to_vec(),
                    },
                    File {
                        path: CFG.to_owned(),
                        bytes: cfg.as_bytes().to_vec(),
                    },
                ],
                Limits::default(),
            )
            .unwrap()
        };
        let keys_of = |c: &Contents| {
            Keys::new(
                Engine::Tlc,
                &crate::id::manifest("M", c, &[], "tlc J"),
                c,
                &[],
                "M",
                "x.cfg",
            )
        };
        let a = keys_of(&contents("cfg1", "m"));
        let cfg_edit = keys_of(&contents("cfg2", "m"));
        let spec_edit = keys_of(&contents("cfg1", "m2"));
        assert_eq!(a.spec_hash, cfg_edit.spec_hash);
        assert_ne!(a.cfg_hash, cfg_edit.cfg_hash);
        assert_ne!(a.spec_hash, spec_edit.spec_hash);
        assert_eq!(a.cfg_hash, spec_edit.cfg_hash);
    }

    #[test]
    fn append_writes_each_end_once_and_load_skips_torn_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        assert_eq!(History::load(&path).unwrap(), History::default());
        let r = record("v1", keys("s", "c", "JA.cfg"), Some(Expected::Long), false);
        assert!(History::append(&path, &r).unwrap());
        assert!(!History::append(&path, &r).unwrap());
        let write = |bytes: &[u8]| {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        };
        // Not UTF-8, then a write cut off with no newline, as a crash leaves it.
        write(b"{\"id\":\"\xc3\n{\"id\":");
        let r2 = record("v2", keys("s", "c", "JA.cfg"), Some(Expected::Max), false);
        assert!(History::append(&path, &r2).unwrap());
        assert!(!History::append(&path, &r2).unwrap());
        assert_eq!(History::load(&path).unwrap().records(), [r, r2]);
    }

    #[test]
    fn an_unreadable_history_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::create_dir(&path).unwrap();
        History::load(&path).unwrap_err();
        History::append(&path, &record("v1", keys("s", "c", "x"), None, true)).unwrap_err();
    }

    #[test]
    fn a_merged_job_is_found_under_every_name() {
        let mut r = record("v1", keys("s1", "c1", "A.cfg"), Some(Expected::Max), true);
        r.names.push(keys("s1", "c1", "B.cfg"));
        let h = history(vec![r]);
        assert_eq!(
            h.lookup(&keys("s2", "c2", "B.cfg")),
            Some((Expected::Max, Source::Name))
        );
    }
}
