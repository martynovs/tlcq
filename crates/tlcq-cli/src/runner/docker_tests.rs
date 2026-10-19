//! Tests of the `docker` CLI containers, against a script standing in for `docker`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;
use crate::runner::state::tests::{run, running};
use crate::runner::state::Slot;

/// A `docker` that logs its arguments, lists `names` for `ps`, prints the file
/// `stats` (if any) for `stats`, and fails when its first argument is `fail_on`.
fn fake_docker(dir: &Path, names: &str, fail_on: &str) -> DockerCli {
    let program = dir.join("docker");
    let log = dir.join("calls");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> {log}\nif [ \"$1\" = \"{fail_on}\" ]; then echo nope >&2; exit 1; fi\n\
         if [ \"$1\" = ps ]; then printf '{names}'; fi\n\
         if [ \"$1\" = stats ]; then cat {stats} 2>/dev/null; fi\n",
        log = log.display(),
        stats = dir.join("stats").display(),
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    DockerCli {
        program,
        image: "tlcq:test".to_owned(),
        volume: "tlcq-state".to_owned(),
        machine: Machine {
            java: PathBuf::from("java"),
            jars: Vec::new(),
            tlx: PathBuf::from("tlx"),
            tlx_mem_percent: 80,
            e_cpus: "16-31".to_owned(),
            p_cpus: "0-15".to_owned(),
            memory_mb: 10_000,
            reserve_mb: 0,
            heap_percent: 40,
            direct_percent: 50,
            slot_memory_mb: None,
        },
        env: vec![("TLCQ_E_CPUS".to_owned(), "16-31".to_owned())],
        meminfo: dir.join("no-meminfo"),
    }
}

fn calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("calls")).unwrap_or_default()
}

fn attempt(slot: Slot) -> Attempt {
    Attempt::new(2, slot, false)
}

#[test]
fn running_lists_the_labelled_containers() {
    let dir = tempfile::tempdir().unwrap();
    let d = fake_docker(dir.path(), "tlcq-a-v1-1\\ntlcq-b-v1-2\\n", "none");
    let names = d.running().unwrap();
    assert_eq!(
        names,
        BTreeSet::from(["tlcq-a-v1-1".to_owned(), "tlcq-b-v1-2".to_owned()])
    );
    assert_eq!(
        calls(dir.path()),
        "ps --all --filter status=running --filter status=paused --filter label=tlcq.attempt --format {{.Names}}\n"
    );
    let failing = fake_docker(dir.path(), "", "ps");
    assert!(failing.running().unwrap_err().to_string().contains("nope"));
}

#[test]
fn an_attempt_starts_pinned_to_its_slot_s_cpus_with_the_state_volume() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fake_docker(dir.path(), "", "none");
    d.start(
        "tlcq-a-v1-2",
        "a",
        &run("v1", running(), "A.cfg"),
        &attempt(Slot::E),
        6500,
    )
    .unwrap();
    assert_eq!(
        calls(dir.path()),
        "run --detach --init --name tlcq-a-v1-2 --label tlcq.attempt=tlcq-a-v1-2 --cpuset-cpus 16-31 --memory 6500m \
         --volume tlcq-state:/state --env TLCQ_E_CPUS=16-31 --env TLCQ_SLOT_MEMORY_MB=6500 tlcq:test tlcq exec a v1 2\n"
    );
    let mut failing = fake_docker(dir.path(), "", "run");
    let err = failing
        .start(
            "n",
            "a",
            &run("v1", running(), "A.cfg"),
            &attempt(Slot::P),
            6500,
        )
        .unwrap_err();
    assert_eq!(err, "nope");
    failing.program = dir.path().join("missing");
    failing
        .start(
            "n",
            "a",
            &run("v1", running(), "A.cfg"),
            &attempt(Slot::P),
            6500,
        )
        .unwrap_err();
}

#[test]
fn stop_and_remove_are_best_effort() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fake_docker(dir.path(), "", "kill");
    d.stop("n1");
    d.remove("n1");
    d.prune_images();
    assert_eq!(
        calls(dir.path()),
        "kill n1\nrm --force n1\nimage prune --force\n"
    );
    d.program = dir.path().join("missing");
    d.remove("n1");
}

#[test]
fn a_container_is_paused_unpaused_and_moved_between_slots() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fake_docker(dir.path(), "", "none");
    d.pause("n1").unwrap();
    d.move_to("n1", Slot::P).unwrap();
    d.unpause("n1").unwrap();
    assert_eq!(
        calls(dir.path()),
        "pause n1\nupdate --cpuset-cpus 0-15 n1\nunpause n1\n"
    );
    let mut failing = fake_docker(dir.path(), "", "pause");
    assert_eq!(failing.pause("n1").unwrap_err(), "nope");
    failing.program = dir.path().join("missing");
    failing.unpause("n1").unwrap_err();
}

#[test]
fn an_attempt_s_memory_is_its_slot_s_share_cut_to_what_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let none = BTreeSet::new();
    // No meminfo to read: the share alone (half of 10,000 MB, all of it alone).
    let d = fake_docker(dir.path(), "", "none");
    assert_eq!(d.memory(Slot::E, false, &none), Some(5000));
    assert_eq!(d.memory(Slot::E, true, &none), Some(10_000));
    assert_eq!((d.workers(Slot::E), d.workers(Slot::P)), (16, 16));
    assert_eq!(d.free_mb(), None);
    assert_eq!(d.room_mb(), None);
    // 9 GB free and nothing running: the share fits.
    let mut d = d;
    d.meminfo = dir.path().join("meminfo");
    std::fs::write(
        &d.meminfo,
        "MemTotal: 10240000 kB\nMemAvailable: 9216000 kB\n",
    )
    .unwrap();
    assert_eq!(d.memory(Slot::E, true, &none), Some(9000));
    assert_eq!(d.free_mb(), Some(9000));
    // A running attempt may still grow 3 GiB: only what is left of the free.
    let mut busy = fake_docker(dir.path(), "tlcq-b-v1-1\\n", "none");
    busy.meminfo = d.meminfo.clone();
    std::fs::write(dir.path().join("stats"), "1GiB / 4GiB\n").unwrap();
    assert_eq!(busy.memory(Slot::E, true, &none), Some(9000 - 3072));
    assert_eq!(busy.room_mb(), Some(9000 - 3072));
    assert!(calls(dir.path()).contains("stats --no-stream --format {{.MemUsage}} tlcq-b-v1-1"));
    // Too little left: it waits; unless the attempt that may grow is frozen.
    std::fs::write(dir.path().join("stats"), "1GiB / 8GiB\n").unwrap();
    assert_eq!(busy.memory(Slot::E, true, &none), None);
    let frozen = BTreeSet::from(["tlcq-b-v1-1".to_owned()]);
    assert_eq!(busy.memory(Slot::E, true, &frozen), Some(9000));
    // podman cannot be asked: nothing is counted as promised.
    let mut deaf = fake_docker(dir.path(), "tlcq-b-v1-1\\n", "stats");
    deaf.meminfo = d.meminfo.clone();
    assert_eq!(deaf.memory(Slot::E, false, &none), Some(5000));
    // A share below the least an attempt starts with never starts.
    deaf.machine.memory_mb = 6000;
    assert_eq!(deaf.memory(Slot::P, false, &none), None);
}

#[test]
fn with_no_list_of_running_containers_nothing_is_counted_as_promised() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = fake_docker(dir.path(), "", "ps");
    d.meminfo = dir.path().join("meminfo");
    std::fs::write(&d.meminfo, "MemAvailable: 9216000 kB\n").unwrap();
    assert_eq!(d.memory(Slot::E, true, &BTreeSet::new()), Some(9000));
}
