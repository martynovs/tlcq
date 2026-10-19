//! Tests of the submit report.

use std::collections::BTreeMap;

use super::*;
use crate::batch::Named;
use crate::commit::PendingFlush;
use crate::config::JobOptions;
use crate::history::Keys;
use crate::job::{Contents, Limits};
use crate::plan::{Kind, Plan, Planned, Route};
use crate::runner::state::tests::entry;

fn job(id: &str, name: &str) -> Job {
    Job {
        id: id.to_owned(),
        engine: crate::engine::Engine::Tlc,
        manifest: String::new(),
        contents: Contents::new(Vec::new(), Limits::default()).unwrap(),
        names: vec![Named {
            name: name.to_owned(),
            keys: Keys {
                engine: crate::engine::Engine::Tlc,
                spec_hash: String::new(),
                cfg_hash: String::new(),
                options_hash: String::new(),
                spec: "M".to_owned(),
                cfg_name: name.to_owned(),
            },
            options: JobOptions::default(),
        }],
    }
}

fn planned(id: &str, lane: Lane) -> Planned {
    Planned {
        id: id.to_owned(),
        lane,
        source: None,
        kind: Kind::New,
        route: Route::VmQueue,
        vm_only: false,
    }
}

fn result(outcome: Outcome) -> JobResult {
    JobResult {
        distinct: Some(6),
        duration_secs: 5,
        ..crate::runner::state::State::outcome_only(outcome)
    }
}

#[test]
fn every_placement_has_its_line() {
    let ids = [
        "a0000000000001",
        "b0000000000002",
        "c3",
        "d4",
        "e5",
        "f6",
        "g7",
        "h8",
        "i9",
        "j10",
    ];
    let jobs: Vec<Job> = ids.iter().map(|id| job(id, &format!("{id}.cfg"))).collect();
    let expected = [Lane::Short, Lane::Unknown, Lane::Long, Lane::Max];
    let placements = [
        Placed::Vm(JobAnswer::Started {
            run: "v1".to_owned(),
        }),
        Placed::Vm(JobAnswer::Queued {
            run: "v2".to_owned(),
        }),
        Placed::Vm(JobAnswer::Attached {
            run: "v3".to_owned(),
        }),
        Placed::Vm(JobAnswer::Refused),
        Placed::Cached(result(Outcome::Ok)),
        Placed::Laptop("l1".to_owned()),
        Placed::LaptopAttach("l2".to_owned()),
        Placed::Pending(RunRef::Token("t".to_owned())),
        Placed::NotStarted,
    ];
    let mut report = Report {
        plan: Plan {
            jobs: ids
                .iter()
                .enumerate()
                .map(|(i, id)| planned(id, expected[i % 4]))
                .collect(),
            skipped: vec!["j10".to_owned()],
            warnings: vec!["x.cfg: its suffix says short".to_owned()],
            ..Plan::default()
        },
        exit: 3,
        ..Report::default()
    };
    for (id, p) in ids.iter().zip(placements) {
        report.placed.insert((*id).to_owned(), p);
    }
    report.dropped.push(PendingFlush {
        id: "a0000000000001".to_owned(),
        run: RunRef::Run("v1".to_owned()),
        entry: entry("A.cfg"),
    });
    let cached = BTreeMap::from([("k11".to_owned(), result(Outcome::Unexpected))]);
    let text = render(&report, &jobs, &[job("k11", "K_max.cfg")], &cached);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "a0000000000001.cfg  a00000000000  vm v1 started  (tlc, short)"
    );
    assert_eq!(
        lines[1],
        "b0000000000002.cfg  b00000000000  vm v2 queued  (tlc, unknown length)"
    );
    for want in [
        "attached to vm v3  (tlc, long)",
        "refused by the vm  (tlc, max)",
        "cached: ok, 6 distinct states, 5s on vm  (tlc, short)",
        "laptop l1",
        "attached to laptop l2",
        "the VM is unreachable: pending on a run not yet known",
        "not started",
        "j10.cfg  j10  not placed",
        "j10.cfg  j10  skipped as max",
        "K_max.cfg  k11  skipped as max (--all, or name it, to run it); cached: unexpected, 6 distinct states",
        "warning: x.cfg: its suffix says short",
        "dropped: a pending entry on a00000000000 v1",
        "the commit's outcome is unknown",
    ] {
        assert!(text.contains(want), "{want:?} not in\n{text}");
    }
}

#[test]
fn a_refused_or_invalid_command_places_nothing() {
    let report = Report {
        plan: Plan {
            jobs: vec![planned("a", Lane::Long)],
            refusals: vec![
                Refusal::JarMismatch,
                Refusal::TlxMismatch,
                Refusal::RunnerOutdated,
                Refusal::LongOffline { id: "a".to_owned() },
                Refusal::OlderRunning {
                    id: "a".to_owned(),
                    old: "b".to_owned(),
                },
                Refusal::ReplaceUnreachable {
                    id: "a".to_owned(),
                    old: "b".to_owned(),
                },
            ],
            ..Plan::default()
        },
        exit: 2,
        ..Report::default()
    };
    let text = render(&report, &[job("a", "A.cfg")], &[], &BTreeMap::new());
    assert_eq!(text.lines().count(), 7);
    assert!(
        text.contains("refused: the VM's tlx rev differs from the laptop's"),
        "{text}"
    );
    assert!(
        text.contains("refused: the VM runs an older tlcq"),
        "{text}"
    );
    assert!(
        text.contains("refused: the VM's TLC jars differ")
            && text.ends_with("nothing was placed\n")
    );
    assert!(!text.contains("A.cfg"));
    let invalid = Report {
        invalid: Some(("a".to_owned(), "its id is x".to_owned())),
        ..Report::default()
    };
    assert_eq!(
        render(&invalid, &[], &[], &BTreeMap::new()),
        "invalid: a: its id is x; nothing was placed\n"
    );
}

#[test]
fn outcomes_read_as_reports_name_them() {
    let cancelled = Outcome::Cancelled {
        by: "ui".to_owned(),
        replaced_by: Some("0123456789abcdef".to_owned()),
    };
    assert_eq!(
        outcome(&cancelled),
        "cancelled by ui, replaced by 0123456789ab"
    );
    let by = Outcome::Cancelled {
        by: "ui".to_owned(),
        replaced_by: None,
    };
    assert_eq!(outcome(&by), "cancelled by ui");
    assert_eq!(
        outcome(&Outcome::Error {
            reason: Some("oom".to_owned())
        }),
        "error: oom"
    );
    assert_eq!(outcome(&Outcome::Error { reason: None }), "error");
    for (o, s) in [
        (Outcome::Unexpected, "unexpected"),
        (Outcome::Timeout, "timeout"),
        (Outcome::NeverSent, "never sent"),
    ] {
        assert_eq!(outcome(&o), s);
    }
    let bare = crate::runner::state::State::outcome_only(Outcome::Ok);
    assert_eq!(ended(&bare), "ok, 0s on vm");
    let noted = JobResult {
        note: Some("TLC reported X, not Y".to_owned()),
        ..crate::runner::state::State::outcome_only(Outcome::Unexpected)
    };
    assert_eq!(ended(&noted), "unexpected, 0s on vm; TLC reported X, not Y");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one case per state, read top to bottom"
)]
fn a_status_line_gives_each_state() {
    use crate::proto::Counters;
    use crate::status::{Line, Status, View};
    let job = |state, place: Option<&str>, elapsed, counters| crate::proto::VmJob {
        state,
        run: Some("v1".to_owned()),
        entries: Vec::new(),
        place: place.map(str::to_owned),
        elapsed_secs: elapsed,
        counters,
        lanes: Vec::new(),
        flags: crate::proto::Flags::default(),
        curve: None,
        position: None,
        left_secs: None,
        cancelled: None,
        started_at: None,
        expected: None,
        waits_for: None,
        held: None,
    };
    let counters = Counters {
        generated: 3_000,
        distinct: 1_000,
        queue: 310,
    };
    let views = [
        View::Vm(crate::proto::VmJob {
            lanes: vec![crate::outcome::LaneDecision {
                to: crate::outcome::Lane::Max,
                after_secs: 1_800,
                probe: None,
            }],
            ..job(VmState::Running, Some("vm E"), Some(3_900), Some(counters))
        }),
        View::Vm(job(
            VmState::Running,
            None,
            None,
            Some(Counters {
                distinct: 0,
                ..counters
            }),
        )),
        View::Vm(job(VmState::Queued, None, None, None)),
        View::Vm(job(VmState::Stopping, None, None, None)),
        View::LastKnown(crate::store::State::Vm),
        View::Laptop(crate::laptop_runs::Progress::default()),
        View::Laptop(crate::laptop_runs::Progress {
            phase: crate::laptop_runs::Phase::Slot { ahead: 2 },
            ..Default::default()
        }),
        View::Laptop(crate::laptop_runs::Progress {
            phase: crate::laptop_runs::Phase::Disk { free_gb: 3 },
            ..Default::default()
        }),
        View::Laptop(crate::laptop_runs::Progress {
            phase: crate::laptop_runs::Phase::Running,
            elapsed_secs: 65,
            counters: Some(counters),
            started_at: Some(9 * 3600 + 30 * 60),
            left_secs: Some((235, 300)),
            generated_per_min: Some(2000),
            queue_growth_permille: Some(125),
        }),
        View::Ended(result(Outcome::Timeout)),
        View::Sent,
        View::NeverSent,
        View::Unknown,
    ];
    let status = Status {
        lines: views
            .into_iter()
            .map(|view| Line {
                key: Some(entry("A.cfg").key),
                id: "0123456789abcdef".to_owned(),
                run: Some("v1".to_owned()),
                view,
                eta: None,
                engine: None,
            })
            .chain(std::iter::once(Line {
                key: None,
                id: "x".to_owned(),
                run: None,
                view: View::Unknown,
                eta: None,
                engine: None,
            }))
            .collect(),
        unreachable: true,
        vm_error: None,
        vm_disk: None,
        exit: 3,
    };
    let text = render_status(&status);
    for want in [
        "the VM is unreachable",
        "A.cfg  0123456789ab  v1  running on vm E for 1h05m, 1000 distinct, 3000 generated, queue 310 (31% of distinct); lane max since 30m00s",
        "running on the vm, 0 distinct",
        "queued on the vm",
        "stopping: cancelled",
        "on the vm, as last known",
        "v1  waiting for the laptop's slot\n",
        "waiting for the laptop's slot, 2 ahead",
        "waiting for disk on the laptop (3 GB free)",
        "running on the laptop for 1m05s (since 09:30 UTC), 1000 distinct, 3000 generated, queue 310 (31% of distinct), 2000 generated/min, queue grew 12.5% of distinct in 5m; cap 5m00s, 3m55s left",
        "timeout, 6 distinct states, 5s on vm",
        "sent; the commit's outcome is not known yet",
        "never sent",
        "-  x  -  error: neither",
    ] {
        assert!(text.contains(want), "{want:?} not in\n{text}");
    }
    assert_eq!(
        (duration(9), duration(260), duration(3_900)),
        ("9s".to_owned(), "4m20s".to_owned(), "1h05m".to_owned())
    );
}

#[test]
fn a_poll_that_kept_failing_says_why() {
    use crate::status::Status;
    let status = Status {
        unreachable: true,
        vm_error: Some("dropped".to_owned()),
        ..Status::default()
    };
    assert_eq!(
        render_status(&status),
        "the VM call failed (dropped): its jobs show their last known state\n"
    );
    assert_eq!(render_status(&Status::default()), "");
}

#[test]
fn a_low_vm_disk_is_reported_and_one_with_room_is_not() {
    use crate::proto::Disk;
    use crate::status::Status;
    let with = |free_mb| Status {
        vm_disk: Some(Disk {
            free_mb,
            floor_mb: 20 * 1024,
        }),
        ..Status::default()
    };
    assert_eq!(
        render_status(&with(15 * 1024)),
        "the VM's disk is low: 15 GB free, no job starts below 20 GB\n"
    );
    assert_eq!(render_status(&with(40 * 1024)), "");
}

#[test]
fn the_checkpoints_paused_runs_keep_are_listed_for_deleting() {
    use crate::status::{Line, Status};
    let line = |id: &str, held| {
        let mut job = vm_job(crate::proto::VmState::Running);
        job.held = held;
        Line {
            key: Some(entry("A.cfg").key),
            id: id.to_owned(),
            run: Some("v1".to_owned()),
            view: View::Vm(job),
            eta: None,
            engine: None,
        }
    };
    let paused = |checkpoint_mb| {
        Some(Held::Paused {
            at: None,
            died: None,
            checkpoint_mb,
        })
    };
    let status = Status {
        lines: vec![
            line("aaaaaaaaaaaa0000", paused(Some(250_000))),
            line("bbbbbbbbbbbb0000", None),
            line("cccccccccccc0000", paused(Some(512))),
            line("dddddddddddd0000", paused(None)),
        ],
        ..Status::default()
    };
    let text = render_status(&status);
    let tail: Vec<&str> = text.lines().skip(4).collect();
    assert_eq!(
        tail,
        [
            "checkpoints kept on the VM's disk for these paused runs, 244 GB in all; `tlcq cancel <id>` deletes one:",
            "  A.cfg  aaaaaaaaaaaa  244 GB",
            "  A.cfg  cccccccccccc  512 MB",
            "  A.cfg  dddddddddddd  0 MB",
        ],
        "{text}"
    );
    let running = Status {
        lines: vec![line("bbbbbbbbbbbb0000", None)],
        ..Status::default()
    };
    assert!(!render_status(&running).contains("checkpoints kept"));
}

#[test]
fn supersedes_are_reported_once_done_and_refusals_say_where_the_older_one_is() {
    use crate::plan::Supersede;
    let sup = |old: &str, on_laptop, attached| Supersede {
        old: old.to_owned(),
        key: entry("A.cfg").key,
        on_laptop,
        by: "new0000000000000".to_owned(),
        attached,
    };
    let report = |exit, invalid: Option<(String, String)>| Report {
        plan: Plan {
            supersede: vec![
                sup("lap000000000000", true, false),
                sup("vm0000000000000", false, true),
            ],
            ..Plan::default()
        },
        exit,
        invalid,
        ..Report::default()
    };
    let done = render(&report(0, None), &[], &[], &BTreeMap::new());
    assert!(done.contains("superseded: A.cfg  lap000000000 on the laptop: cancelled unless another submitter still holds it; replaced by new000000000"), "{done}");
    assert!(
        done.contains(
            "superseded: A.cfg  vm0000000000 on the vm: kept: this command attaches to it"
        ),
        "{done}"
    );
    let unknown = render(&report(3, None), &[], &[], &BTreeMap::new());
    assert!(unknown.contains("maybe superseded"), "{unknown}");
    let invalid = render(
        &report(2, Some(("x".to_owned(), "jars".to_owned()))),
        &[],
        &[],
        &BTreeMap::new(),
    );
    assert!(
        invalid.contains("on the laptop") && !invalid.contains("on the vm"),
        "{invalid}"
    );
    let older = crate::proto::VmOlder {
        id: "old".to_owned(),
        key: entry("A.cfg").key,
        running: true,
        place: "vm E".to_owned(),
        elapsed_secs: 600,
        counters: Some(crate::proto::Counters {
            generated: 30,
            distinct: 10,
            queue: 4,
        }),
    };
    let refused = Report {
        plan: Plan {
            refusals: vec![Refusal::OlderRunning {
                id: "new".to_owned(),
                old: "old".to_owned(),
            }],
            ..Plan::default()
        },
        older: vec![older.clone()],
        exit: 2,
        ..Report::default()
    };
    let text = render(&refused, &[], &[], &BTreeMap::new());
    assert!(text.contains("an older version, old, is running on vm E for 10m00s, 10 distinct, queue 4 (--replace supersedes it)"), "{text}");
    let bare = Report {
        older: vec![crate::proto::VmOlder {
            counters: None,
            ..older
        }],
        ..refused
    };
    assert!(render(&bare, &[], &[], &BTreeMap::new()).contains("for 10m00s (--replace"));
}

#[test]
fn a_running_line_gives_its_estimated_end_and_its_source() {
    let line = |source| crate::status::Line {
        key: None,
        id: "0123456789abcdef".to_owned(),
        run: Some("v1".to_owned()),
        view: View::Sent,
        eta: Some(crate::status::LineEta {
            remaining_secs: 4200,
            source,
        }),
        engine: Some(crate::engine::Engine::Tlx),
    };
    let status = Status {
        lines: [
            crate::eta::Source::Previous,
            crate::eta::Source::Edited,
            crate::eta::Source::FirstRun,
        ]
        .map(line)
        .to_vec(),
        ..Status::default()
    };
    let text = render_status(&status);
    for want in [
        "; ends in about 1h10m (from its previous run)  (tlx)\n",
        "(from a run of an earlier version)",
        "(first-run estimate)",
    ] {
        assert!(text.contains(want), "{want:?} not in\n{text}");
    }
}

/// A VM job in `state`, otherwise empty.
fn vm_job(state: crate::proto::VmState) -> crate::proto::VmJob {
    crate::proto::VmJob {
        state,
        run: Some("v1".to_owned()),
        entries: Vec::new(),
        place: None,
        elapsed_secs: None,
        counters: None,
        lanes: Vec::new(),
        flags: crate::proto::Flags::default(),
        curve: None,
        position: None,
        left_secs: None,
        cancelled: None,
        started_at: None,
        expected: None,
        waits_for: None,
        held: None,
    }
}

#[test]
fn a_placement_gives_its_slot_or_queue_place_its_lane_s_source_and_its_cap() {
    use crate::proto::VmState;
    let mut started = job("a0000000000001", "A.cfg");
    started.names[0].options.timeout = Some(std::time::Duration::from_secs(5400));
    let queued = job("b0000000000002", "B.cfg");
    let mut report = Report::default();
    for (j, source) in [(&started, Source::Configuration), (&queued, Source::Suffix)] {
        report.plan.jobs.push(Planned {
            source: Some(source),
            ..planned(&j.id, Lane::Long)
        });
    }
    let run = || "v1".to_owned();
    report.placed.insert(
        started.id.clone(),
        Placed::Vm(JobAnswer::Started { run: run() }),
    );
    report.placed.insert(
        queued.id.clone(),
        Placed::Vm(JobAnswer::Queued { run: run() }),
    );
    report.views.insert(
        started.id.clone(),
        crate::proto::VmJob {
            place: Some("vm E".to_owned()),
            ..vm_job(VmState::Running)
        },
    );
    report.views.insert(
        queued.id.clone(),
        crate::proto::VmJob {
            position: Some(3),
            ..vm_job(VmState::Queued)
        },
    );
    let text = render(&report, &[started, queued], &[], &BTreeMap::new());
    for want in [
        "vm v1 started in E  (tlc, long, from an earlier spec, cap 1h30m)",
        "vm v1 queued, 2 ahead  (tlc, long, by its name's suffix)",
    ] {
        assert!(text.contains(want), "{want:?} not in\n{text}");
    }
    assert_eq!(
        (
            ahead(1),
            source(Some(Source::Exact)),
            source(Some(Source::Name)),
            source(Some(Source::Tlc))
        ),
        (
            "next".to_owned(),
            ", from its history",
            ", from its name's history",
            ", from its runs on TLC"
        )
    );
}

#[test]
fn a_status_line_gives_the_queue_place_the_canceller_rates_and_time_left() {
    use crate::proto::VmState;
    use crate::status::{Line, Status, View};
    let curve = "# tlcq curve\n60\t3\t1000\t500\t100\n360\t5\t31000\t10500\t3100\n";
    let views = [
        View::Vm(crate::proto::VmJob {
            position: Some(1),
            waits_for: Some("disk".to_owned()),
            held: None,
            ..vm_job(VmState::Queued)
        }),
        View::Vm(crate::proto::VmJob {
            run: Some("l1".to_owned()),
            started_at: Some(86_400 + 14 * 3600 + 5 * 60),
            expected: Some((crate::history::Expected::Long, Some(Source::Configuration))),
            ..vm_job(VmState::Running)
        }),
        View::Vm(crate::proto::VmJob {
            expected: Some((crate::history::Expected::Short, None)),
            lanes: vec![crate::outcome::LaneDecision {
                to: crate::outcome::Lane::Slow,
                after_secs: 900,
                probe: None,
            }],
            ..vm_job(VmState::Running)
        }),
        View::Vm(crate::proto::VmJob {
            expected: Some((crate::history::Expected::Max, Some(Source::Flag))),
            ..vm_job(VmState::Running)
        }),
        View::Vm(crate::proto::VmJob {
            lanes: vec![crate::outcome::LaneDecision {
                to: crate::outcome::Lane::Max,
                after_secs: 1800,
                probe: Some(crate::outcome::Probe {
                    distinct: 10,
                    queue: 9,
                    queue_before: 1,
                }),
            }],
            ..vm_job(VmState::Running)
        }),
        View::Vm(crate::proto::VmJob {
            cancelled: Some(("w1".to_owned(), Some("0123456789abcdef".to_owned()))),
            ..vm_job(VmState::Stopping)
        }),
        View::Vm(crate::proto::VmJob {
            left_secs: Some((600, 7200)),
            curve: Some(crate::proto::AttemptCurve {
                attempt: 1,
                text: curve.to_owned(),
            }),
            ..vm_job(VmState::Running)
        }),
        View::Ended(JobResult {
            generated: Some(900),
            depth: Some(12),
            exit_status: Some(12),
            ..result(Outcome::Unexpected)
        }),
    ];
    let status = Status {
        lines: views
            .into_iter()
            .map(|view| Line {
                key: None,
                id: "a".to_owned(),
                run: None,
                view,
                eta: None,
                engine: None,
            })
            .collect(),
        ..Status::default()
    };
    let text = render_status(&status);
    for want in [
        "queued on the vm, next, waiting for disk",
        "running on the vm, moved from the laptop (this attempt since 14:05 UTC); lane long, from an earlier spec",
        "lane slow since 15m00s, still running past the short lane",
        "lane max, by flag",
        "lane max since 30m00s, by the size probe",
        "stopping: cancelled by w1, replaced by 0123456789ab",
        "6000 generated/min, queue grew 28.6% of distinct in 5m; lane unknown length; cap 2h00m, 10m00s left",
        "unexpected, 6 distinct states, 900 generated, depth 12, exit 12, 5s on vm",
    ] {
        assert!(text.contains(want), "{want:?} not in\n{text}");
    }
}

#[test]
fn a_temporal_must_fail_on_tlx_gets_the_hint_to_try_tlc() {
    let with_cfg = |id: &str, name: &str, engine, cfg: &str| Job {
        engine,
        contents: Contents::new(
            vec![crate::job::File {
                path: crate::job::CFG.to_owned(),
                bytes: cfg.as_bytes().to_vec(),
            }],
            Limits::default(),
        )
        .unwrap(),
        ..job(id, name)
    };
    let live = "\\* must fail Settles\nSPECIFICATION Spec\nPROPERTY Settles\n";
    let safe = "\\* must fail Safe\nSPECIFICATION Spec\nINVARIANT Safe\n";
    let jobs = [
        with_cfg("a1", "NB_08_live.cfg", crate::engine::Engine::Tlx, live),
        with_cfg("b2", "NB_15.cfg", crate::engine::Engine::Tlx, safe),
        with_cfg("c3", "NB_09_live2.cfg", crate::engine::Engine::Tlc, live),
        with_cfg("d4", "NB_10_live3.cfg", crate::engine::Engine::Tlx, live),
    ];
    let mut cache_hit = planned("d4", Lane::Short);
    cache_hit.kind = Kind::CacheHit(result(Outcome::Ok));
    let report = Report {
        plan: Plan {
            jobs: vec![
                planned("a1", Lane::Short),
                planned("b2", Lane::Short),
                planned("c3", Lane::Short),
                cache_hit,
            ],
            ..Plan::default()
        },
        ..Report::default()
    };
    let text = render(&report, &jobs, &[], &BTreeMap::new());
    let hints: Vec<&str> = text.lines().filter(|l| l.starts_with("hint: ")).collect();
    assert_eq!(
        hints,
        [
            "hint: NB_08_live.cfg must fail a temporal property; tlx checks liveness after the full search, --engine tlc --replace stops at the violation",
            "hint: NB_10_live3.cfg must fail a temporal property; tlx checks liveness after the full search, --engine tlc stops at the violation",
        ],
        "{text}"
    );
}

#[test]
fn a_frozen_pausing_or_paused_job_says_so() {
    let held = |held, place: &str| {
        let mut job = vm_job(crate::proto::VmState::Running);
        job.place = Some(place.to_owned());
        job.held = held;
        view(&View::Vm(job))
    };
    let at = Some(1_760_000_000);
    assert!(held(None, "vm E").starts_with("running on vm E;"));
    assert!(held(
        Some(Held::Frozen {
            since: at,
            spare: false
        }),
        "vm E"
    )
    .starts_with("frozen on the vm since "));
    assert!(held(
        Some(Held::Frozen {
            since: None,
            spare: true
        }),
        "vm P"
    )
    .starts_with("running on vm P on spare capacity, frozen;"));
    assert!(held(Some(Held::Pausing), "vm E")
        .starts_with("running on vm E, pausing at its next checkpoint"));
    let paused = |at, died: Option<&str>, checkpoint_mb| {
        Some(Held::Paused {
            at,
            died: died.map(str::to_owned),
            checkpoint_mb,
        })
    };
    assert!(
        held(paused(at, None, None), "vm E").starts_with("paused on the vm at its checkpoint of ")
    );
    assert!(
        held(paused(None, None, None), "vm E").starts_with("paused on the vm at its checkpoint;")
    );
    let died = "stopped: killed (signal 9), most likely out of memory";
    let text = held(paused(at, Some(died), Some(250_000)), "vm E");
    assert!(
        text.starts_with("paused on the vm: it stopped at "),
        "{text}"
    );
    assert!(text.contains(
        " UTC (stopped: killed (signal 9), most likely out of memory); its checkpoint (244 GB) is kept: `tlcq resume` recovers it (`--memory` for more memory, `--timeout` for a longer cap), `tlcq cancel` drops it"
    ), "{text}");
    assert!(held(paused(None, None, Some(512)), "vm E")
        .starts_with("paused on the vm at its checkpoint;"));
    assert!(held(paused(None, Some(died), None), "vm E")
        .starts_with("paused on the vm: it stopped (stopped: killed (signal 9), most likely out of memory); its checkpoint is kept:"));
}

#[test]
fn a_waiting_or_unpaused_job_names_no_slot_and_an_unknown_hold_still_reads() {
    let held = |held| {
        let mut job = vm_job(crate::proto::VmState::Running);
        job.held = Some(held);
        view(&View::Vm(job))
    };
    assert!(held(Held::Waiting)
        .starts_with("waiting first in the queue for a VM slot, resumed or unfrozen;"));
    assert!(held(Held::Pausing).starts_with("pausing on the vm, stopping at once;"));
    assert!(held(Held::Other).starts_with("held on the vm;"));
    // A newer runner's hold reads as one this laptop does not know.
    let job: crate::proto::VmJob = serde_json::from_value(serde_json::json!({
        "state": "running", "run": "v1", "entries": [], "held": { "held": "hibernating" }
    }))
    .unwrap();
    assert_eq!(job.held, Some(Held::Other));
    assert!(!Held::Other.runs() && !Held::Waiting.runs());
    assert!(
        Held::Pausing.runs()
            && Held::Frozen {
                since: None,
                spare: true
            }
            .runs()
    );
}

#[test]
fn a_tlx_run_reports_what_is_left_of_its_level_beside_the_queue() {
    let line = |secs, depth, distinct, queue| {
        format!(
            r#"{{"kind":"progress","secs":{secs},"depth":{depth},"generated":0,"distinct":{distinct},"queue":{queue}}}"#
        )
    };
    let curve = [
        line(0, 23, 127, 3),
        line(60, 24, 130, 48),
        line(120, 24, 137, 44),
        line(180, 24, 144, 40),
    ]
    .join("\n");
    let job = crate::proto::VmJob {
        curve: Some(crate::proto::AttemptCurve {
            attempt: 1,
            text: curve,
        }),
        ..vm_job(VmState::Running)
    };
    // Beside its estimated end.
    let line = Line {
        key: None,
        id: "a".to_owned(),
        run: Some("v1".to_owned()),
        view: View::Vm(job),
        eta: Some(LineEta {
            remaining_secs: 3600,
            source: EtaSource::FirstRun,
        }),
        engine: None,
    };
    let status = Status {
        lines: vec![line],
        ..Status::default()
    };
    let text = render_status(&status);
    assert!(
        text.contains("; ends in about 1h00m (first-run estimate), level 24: 40 left (-4/min)\n"),
        "{text}"
    );
    // Not a VM job, or no curve: nothing.
    assert_eq!(level(&View::Sent), "");
    assert_eq!(level(&View::Vm(vm_job(VmState::Running))), "");
}
