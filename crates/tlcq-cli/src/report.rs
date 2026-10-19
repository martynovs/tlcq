//! What `submit` tells the agent (design D1): one line per job, with where it
//! went, then refusals, skips, warnings and anything left to resolve.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::batch::Job;
use crate::commit::JobAnswer;
use crate::curve::Curve;
use crate::engine::Engine;
use crate::eta::Source as EtaSource;
use crate::history::{Expected, Source};
use crate::laptop_runs::{Phase, Progress};
use crate::outcome::{JobResult, Outcome};
use crate::plan::{Kind, Lane, Refusal};
use crate::proto::{Counters, Held, VmJob, VmOlder, VmState};
use crate::status::{Line, LineEta, Status, View};
use crate::store::RunRef;
use crate::submit::{Placed, Report};

/// The first 12 characters of an id, enough to tell jobs apart in a report.
pub fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// The report of `report`, for the jobs `jobs` and those `skipped` as max by
/// their suffix; `cached` holds the laptop's results for skipped ids, shown on
/// their lines.
pub fn render(
    report: &Report,
    jobs: &[Job],
    skipped: &[Job],
    cached: &BTreeMap<String, JobResult>,
) -> String {
    let job = |id: &str| jobs.iter().chain(skipped).find(|j| j.id == id);
    let names = |id: &str| {
        job(id)
            .map(|j| {
                j.names
                    .iter()
                    .map(|n| n.name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default()
    };
    let mut out = String::new();
    let mut line = |s: String| writeln!(out, "{s}").expect("writing to a string");
    for r in &report.plan.refusals {
        line(format!("refused: {}", refusal(r, &report.older)));
    }
    if !report.plan.refusals.is_empty() {
        line("nothing was placed".to_owned());
    }
    if let Some((id, reason)) = &report.invalid {
        line(format!(
            "invalid: {}: {reason}; nothing was placed",
            short(id)
        ));
    }
    if report.plan.refusals.is_empty() && report.invalid.is_none() {
        for p in &report.plan.jobs {
            let view = report.views.get(&p.id);
            let placed = report
                .placed
                .get(&p.id)
                .map_or_else(|| "not placed".to_owned(), |pl| placed(pl, view));
            let cap = job(&p.id)
                .and_then(|j| j.names.iter().filter_map(|n| n.options.timeout).max())
                .map(|t| format!(", cap {}", duration(t.as_secs())))
                .unwrap_or_default();
            let engine = job(&p.id).map_or("?", |j| j.engine.name());
            line(format!(
                "{}  {}  {placed}  ({engine}, {}{}{cap})",
                names(&p.id),
                short(&p.id),
                lane(p.lane),
                source(p.source),
            ));
        }
        for hint in temporal_hints(report, jobs) {
            line(hint);
        }
    }
    for id in report
        .plan
        .skipped
        .iter()
        .chain(skipped.iter().map(|j| &j.id))
    {
        let known = cached
            .get(id)
            .map_or_else(String::new, |r| format!("; cached: {}", ended(r)));
        line(format!(
            "{}  {}  skipped as max (--all, or name it, to run it){known}",
            names(id),
            short(id)
        ));
    }
    if report.plan.refusals.is_empty() {
        for s in &report.plan.supersede {
            // Laptop supersedes happen before the commit; the VM's in it.
            let done = match (s.on_laptop, report.exit, &report.invalid) {
                (true, _, _) | (false, 0, _) => "superseded",
                (false, 3, _) => "maybe superseded (the commit's outcome is unknown)",
                (false, _, _) => continue,
            };
            let fate = if s.attached {
                "kept: this command attaches to it"
            } else {
                "cancelled unless another submitter still holds it"
            };
            line(format!(
                "{done}: {}  {} on the {}: {fate}; replaced by {}",
                s.key.cfg_name,
                short(&s.old),
                if s.on_laptop { "laptop" } else { "vm" },
                short(&s.by)
            ));
        }
    }
    for w in &report.plan.warnings {
        line(format!("warning: {w}"));
    }
    for d in &report.dropped {
        line(format!(
            "dropped: a pending entry on {} {}: its run has ended or the VM does not know it",
            short(&d.id),
            run_ref(&d.run)
        ));
    }
    if report.exit == 3 {
        line("the commit's outcome is unknown: `tlcq status` resolves it; nothing started on the laptop".to_owned());
    }
    out
}

/// A hint per tlx job that must fail a temporal property: TLC stops at the
/// violation, tlx explores everything first (tlx-engine E6).
fn temporal_hints(report: &Report, jobs: &[Job]) -> Vec<String> {
    let mut hints = Vec::new();
    for p in &report.plan.jobs {
        let Some(j) = jobs
            .iter()
            .find(|j| j.id == p.id && j.engine == Engine::Tlx && j.temporal_must_fail())
        else {
            continue;
        };
        // Run on TLC, it is a new version of the configuration: one still
        // queued or running here needs --replace.
        let flags = if matches!(p.kind, Kind::CacheHit(_)) {
            "--engine tlc"
        } else {
            "--engine tlc --replace"
        };
        let names: Vec<&str> = j.names.iter().map(|n| n.name.as_str()).collect();
        hints.push(format!(
            "hint: {} must fail a temporal property; tlx checks liveness after the full search, {flags} stops at the violation",
            names.join(",")
        ));
    }
    hints
}

/// `status`'s report: one line per run, the VM's reachability first.
pub fn render_status(status: &Status) -> String {
    let mut out = String::new();
    match &status.vm_error {
        Some(e) => writeln!(
            out,
            "the VM call failed ({e}): its jobs show their last known state"
        )
        .expect("writing to a string"),
        None if status.unreachable => {
            out.push_str("the VM is unreachable: its jobs show their last known state\n");
        }
        None => {}
    }
    if let Some(d) = status.vm_disk.filter(crate::proto::Disk::low) {
        writeln!(
            out,
            "the VM's disk is low: {} GB free, no job starts below {} GB",
            d.free_mb >> 10,
            d.floor_mb >> 10
        )
        .expect("writing to a string");
    }
    for l in &status.lines {
        let cfg = l.key.as_ref().map_or("-", |k| k.cfg_name.as_str());
        let run = l.run.as_deref().unwrap_or("-");
        let eta = l.eta.map(eta).unwrap_or_default();
        let level = level(&l.view);
        let engine = l.engine.map(|e| format!("  ({e})")).unwrap_or_default();
        writeln!(
            out,
            "{cfg}  {}  {run}  {}{eta}{level}{engine}",
            short(&l.id),
            view(&l.view)
        )
        .expect("writing to a string");
    }
    kept(&mut out, &status.lines);
    out
}

/// A running tlx job's level, beside its estimated end: what is left of it,
/// draining while the queue (with the next level found so far) may grow.
fn level(v: &View) -> String {
    let View::Vm(job) = v else {
        return String::new();
    };
    let Some(l) = job
        .curve
        .as_ref()
        .and_then(|c| Curve::parse(&c.text).level(300.0))
    else {
        return String::new();
    };
    let mut s = format!(", level {}: {:.0} left", l.depth, l.left);
    if let Some(m) = l.per_min {
        write!(s, " ({m:+.0}/min)").expect("writing to a string");
    }
    s
}

/// The checkpoints the paused runs among `lines` keep on the VM's disk, each
/// one's cancel deleting it.
fn kept(out: &mut String, lines: &[Line]) {
    let kept: Vec<_> = lines
        .iter()
        .filter_map(|l| match &l.view {
            View::Vm(VmJob {
                held: Some(Held::Paused { checkpoint_mb, .. }),
                ..
            }) => Some((l, checkpoint_mb.unwrap_or(0))),
            _ => None,
        })
        .collect();
    if kept.is_empty() {
        return;
    }
    let total = kept.iter().map(|(_, mb)| mb).sum::<u64>();
    writeln!(
        out,
        "checkpoints kept on the VM's disk for these paused runs, {} in all; `tlcq cancel <id>` deletes one:",
        size(total)
    )
    .expect("writing to a string");
    for (l, mb) in kept {
        let cfg = l.key.as_ref().map_or("-", |k| k.cfg_name.as_str());
        writeln!(out, "  {cfg}  {}  {}", short(&l.id), size(mb)).expect("writing to a string");
    }
}

/// A size in MB, as GB from 1 GB up.
fn size(mb: u64) -> String {
    if mb >= 1024 {
        format!("{} GB", mb >> 10)
    } else {
        format!("{mb} MB")
    }
}

/// A running job's estimated end, and where it comes from.
fn eta(e: LineEta) -> String {
    let source = match e.source {
        EtaSource::Previous => "from its previous run",
        EtaSource::Edited => "from a run of an earlier version",
        EtaSource::FirstRun => "first-run estimate",
    };
    format!("; ends in about {} ({source})", duration(e.remaining_secs))
}

/// A reported run's state.
fn view(v: &View) -> String {
    match v {
        View::Vm(job) => match job.state {
            VmState::Queued => {
                let mut s = "queued on the vm".to_owned();
                if let Some(n) = job.position {
                    write!(s, ", {}", ahead(n)).expect("writing to a string");
                }
                if let Some(what) = &job.waits_for {
                    write!(s, ", waiting for {what}").expect("writing to a string");
                }
                s
            }
            VmState::Stopping => match &job.cancelled {
                Some((by, replaced_by)) => format!(
                    "stopping: {}",
                    outcome(&Outcome::Cancelled {
                        by: by.clone(),
                        replaced_by: replaced_by.clone(),
                    })
                ),
                None => "stopping: cancelled".to_owned(),
            },
            _ => running(job),
        },
        View::LastKnown(_) => "on the vm, as last known".to_owned(),
        View::Laptop(p) => laptop(p),
        View::Ended(r) => ended(r),
        View::Sent => "sent; the commit's outcome is not known yet".to_owned(),
        View::NeverSent => "never sent".to_owned(),
        View::Unknown => "error: neither the laptop nor the VM knows it".to_owned(),
    }
}

/// A running job: where, how the page holds it (frozen, pausing, paused),
/// for how long, and TLC's counters.
fn running(job: &VmJob) -> String {
    let place = job.place.as_deref().unwrap_or("the vm");
    let since = |at: Option<u64>| {
        at.map(|t| format!(" since {} UTC", clock(t)))
            .unwrap_or_default()
    };
    let mut s = match &job.held {
        None => format!("running on {place}"),
        Some(Held::Frozen { since: at, spare }) if *spare => {
            format!("running on {place} on spare capacity, frozen{}", since(*at))
        }
        Some(Held::Frozen { since: at, .. }) => format!("frozen on the vm{}", since(*at)),
        // In no slot: a frozen job unpaused for its pause, which stops at once.
        Some(Held::Pausing) if job.place.is_none() => {
            "pausing on the vm, stopping at once".to_owned()
        }
        Some(Held::Pausing) => format!("running on {place}, pausing at its next checkpoint"),
        Some(Held::Paused {
            at,
            died: Some(why),
            checkpoint_mb,
        }) => format!(
            "paused on the vm: it stopped{} ({why}); its checkpoint{} is kept: `tlcq resume` recovers it (`--memory` for more memory, `--timeout` for a longer cap), `tlcq cancel` drops it",
            at.map(|t| format!(" at {} UTC", clock(t)))
                .unwrap_or_default(),
            checkpoint_mb
                .map(|mb| format!(" ({})", size(mb)))
                .unwrap_or_default()
        ),
        Some(Held::Paused { at, died: None, .. }) => format!(
            "paused on the vm at its checkpoint{}",
            at.map(|t| format!(" of {} UTC", clock(t)))
                .unwrap_or_default()
        ),
        Some(Held::Waiting) => {
            "waiting first in the queue for a VM slot, resumed or unfrozen".to_owned()
        }
        Some(Held::Other) => "held on the vm".to_owned(),
    };
    if job.run.as_deref().is_some_and(|r| r.starts_with('l')) {
        s.push_str(", moved from the laptop");
    }
    if let Some(secs) = job.elapsed_secs {
        write!(s, " for {}", duration(secs)).expect("writing to a string");
    }
    if let Some(at) = job.started_at {
        write!(s, " (this attempt since {} UTC)", clock(at)).expect("writing to a string");
    }
    if let Some(c) = job.counters {
        counters(&mut s, c);
    }
    let curve = job.curve.as_ref().map(|c| Curve::parse(&c.text));
    if let Some(r) = curve.as_ref().and_then(|c| c.rates(300.0)) {
        write!(
            s,
            ", {:.0} generated/min, queue grew {:.1}% of distinct in 5m",
            r.generated_per_min,
            r.queue_growth * 100.0
        )
        .expect("writing to a string");
    }
    match (job.lanes.last(), job.expected) {
        (Some(d), _) => {
            let lane = match d.to {
                crate::outcome::Lane::Slow => "slow",
                crate::outcome::Lane::Max => "max",
            };
            let why = if d.probe.is_some() {
                "by the size probe"
            } else {
                "still running past the short lane"
            };
            write!(s, "; lane {lane} since {}, {why}", duration(d.after_secs))
                .expect("writing to a string");
        }
        (None, Some((expected, from))) => {
            let lane = match expected {
                Expected::Short => "short",
                Expected::Long => "long",
                Expected::Max => "max",
            };
            let from = source(from).trim_start_matches(", ");
            let from = if from.is_empty() { "by flag" } else { from };
            write!(s, "; lane {lane}, {from}").expect("writing to a string");
        }
        (None, None) => s.push_str("; lane unknown length"),
    }
    if let Some(left) = job.left_secs {
        s.push_str(&cap(left));
    }
    s
}

/// A laptop run: waiting for the slot or the disk, or running, for how long,
/// and TLC's counters.
fn laptop(p: &Progress) -> String {
    match p.phase {
        Phase::Slot { ahead: 0 } => "waiting for the laptop's slot".to_owned(),
        Phase::Slot { ahead } => format!("waiting for the laptop's slot, {ahead} ahead"),
        Phase::Disk { free_gb } => {
            format!("waiting for disk on the laptop ({free_gb} GB free)")
        }
        Phase::Running => {
            let mut s = format!("running on the laptop for {}", duration(p.elapsed_secs));
            if let Some(at) = p.started_at {
                write!(s, " (since {} UTC)", clock(at)).expect("writing to a string");
            }
            if let Some(c) = p.counters {
                counters(&mut s, c);
            }
            if let (Some(g), Some(q)) = (p.generated_per_min, p.queue_growth_permille) {
                write!(
                    s,
                    ", {g} generated/min, queue grew {:.1}% of distinct in 5m",
                    f64::from(i32::try_from(q).unwrap_or(i32::MAX)) / 10.0
                )
                .expect("writing to a string");
            }
            if let Some(left) = p.left_secs {
                s.push_str(&cap(left));
            }
            s
        }
    }
}

/// TLC's counters, after a running job's place.
fn counters(s: &mut String, c: Counters) {
    write!(
        s,
        ", {} distinct, {} generated, queue {}",
        c.distinct, c.generated, c.queue
    )
    .expect("writing to a string");
    if let Some(percent) = (c.queue * 100).checked_div(c.distinct) {
        write!(s, " ({percent}% of distinct)").expect("writing to a string");
    }
}

/// A running job's cap, and the time left before it.
fn cap((left, cap): (u64, u64)) -> String {
    format!("; cap {}, {} left", duration(cap), duration(left))
}

/// A time of day, UTC, as `14:05`, from seconds since the epoch.
fn clock(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60)
}

/// Seconds as `1h05m`, `4m20s` or `9s`.
pub fn duration(secs: u64) -> String {
    match (secs / 3600, secs / 60 % 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m{s:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

/// Where a job went; for a VM job, `view` (as the VM had it after the
/// commit) gives its slot or its place in the queue.
fn placed(p: &Placed, view: Option<&VmJob>) -> String {
    let slot = view
        .and_then(|v| v.place.as_deref())
        .map(|place| format!(" in {}", place.trim_start_matches("vm ")))
        .unwrap_or_default();
    let ahead = view
        .and_then(|v| v.position)
        .map(|n| format!(", {}", ahead(n)))
        .unwrap_or_default();
    match p {
        Placed::Cached(result) | Placed::Vm(JobAnswer::Cached { result }) => {
            format!("cached: {}", ended(result))
        }
        Placed::Laptop(run) => format!("laptop {run}"),
        Placed::LaptopAttach(run) => format!("attached to laptop {run}"),
        Placed::Vm(JobAnswer::Started { run }) => format!("vm {run} started{slot}"),
        Placed::Vm(JobAnswer::Queued { run }) => format!("vm {run} queued{ahead}"),
        Placed::Vm(JobAnswer::Attached { run }) => format!("attached to vm {run}"),
        Placed::Vm(JobAnswer::Refused) => "refused by the vm".to_owned(),
        Placed::Pending(run) => format!("the VM is unreachable: pending on {}", run_ref(run)),
        Placed::NotStarted => "not started".to_owned(),
    }
}

/// A run, or the commit whose token will name it.
fn run_ref(r: &RunRef) -> String {
    match r {
        RunRef::Run(run) => run.clone(),
        RunRef::Token(_) => "a run not yet known".to_owned(),
    }
}

/// An ended run: its outcome and, when known, its states.
pub fn ended(r: &JobResult) -> String {
    let mut s = outcome(&r.outcome);
    if let Some(d) = r.distinct {
        write!(s, ", {d} distinct states").expect("writing to a string");
    }
    if let Some(g) = r.generated {
        write!(s, ", {g} generated").expect("writing to a string");
    }
    if let Some(d) = r.depth {
        write!(s, ", depth {d}").expect("writing to a string");
    }
    if let Some(code) = r.exit_status {
        write!(s, ", exit {code}").expect("writing to a string");
    }
    write!(s, ", {}s on {}", r.duration_secs, r.place).expect("writing to a string");
    if let Some(note) = &r.note {
        write!(s, "; {note}").expect("writing to a string");
    }
    s
}

/// An outcome as reports name it.
pub fn outcome(o: &Outcome) -> String {
    match o {
        Outcome::Ok => "ok".to_owned(),
        Outcome::Unexpected => "unexpected".to_owned(),
        Outcome::Timeout => "timeout".to_owned(),
        Outcome::Cancelled { by, replaced_by } => match replaced_by {
            Some(new) => format!("cancelled by {by}, replaced by {}", short(new)),
            None => format!("cancelled by {by}"),
        },
        Outcome::Error { reason } => match reason {
            Some(r) => format!("error: {r}"),
            None => "error".to_owned(),
        },
        Outcome::NeverSent => "never sent".to_owned(),
    }
}

/// A queued run's place, `n` from 1: next, or how many wait ahead of it.
fn ahead(n: usize) -> String {
    match n {
        0 | 1 => "next".to_owned(),
        n => format!("{} ahead", n - 1),
    }
}

/// Where an expectation came from, after its lane.
fn source(s: Option<Source>) -> &'static str {
    match s {
        Some(Source::Suffix) => ", by its name's suffix",
        Some(Source::Exact) => ", from its history",
        Some(Source::Configuration) => ", from an earlier spec",
        Some(Source::Name) => ", from its name's history",
        Some(Source::Tlc) => ", from its runs on TLC",
        Some(Source::Flag) | None => "",
    }
}

/// A lane as reports name it.
fn lane(l: Lane) -> &'static str {
    match l {
        Lane::Short => "short",
        Lane::Unknown => "unknown length",
        Lane::Long => "long",
        Lane::Max => "max",
    }
}

/// Why a command was refused; an older version running on the VM with where
/// and how far it is, so the agent can decide on `--replace`.
fn refusal(r: &Refusal, older: &[VmOlder]) -> String {
    match r {
        Refusal::JarMismatch => {
            "the VM's TLC jars differ from the laptop's (`just docker-jars`, then rebuild the image)"
                .to_owned()
        }
        Refusal::TlxMismatch => {
            "the VM's tlx rev differs from the laptop's: install the same rev on both (`just install`, `just docker-pve`)"
                .to_owned()
        }
        Refusal::RunnerOutdated => {
            "the VM runs an older tlcq, whose job ids differ from this laptop's: redeploy it (`just docker-pve`)"
                .to_owned()
        }
        Refusal::LongOffline { id } => format!(
            "{}: a long job while the VM is unreachable (--local runs it here)",
            short(id)
        ),
        Refusal::MemoryOnLaptop { id } => format!(
            "{}: --memory is for VM runs, and this one would run on the laptop (--local, or the VM unreachable)",
            short(id)
        ),
        Refusal::OlderRunning { id, old } => {
            let mut s = format!(
                "{}: an older version, {}, is running",
                short(id),
                short(old)
            );
            if let Some(o) = older.iter().find(|o| &o.id == old) {
                write!(s, " on {} for {}", o.place, duration(o.elapsed_secs))
                    .expect("writing to a string");
                if let Some(c) = o.counters {
                    write!(s, ", {} distinct, queue {}", c.distinct, c.queue)
                        .expect("writing to a string");
                }
            }
            s.push_str(" (--replace supersedes it)");
            s
        }
        Refusal::ReplaceUnreachable { id, old } => format!(
            "{}: an older version, {}, may be running on the VM, which is unreachable: it cannot be superseded until the VM is back",
            short(id),
            short(old)
        ),
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
