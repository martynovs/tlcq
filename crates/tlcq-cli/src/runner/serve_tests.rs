//! Tests of the verbs' stdin/stdout framing and the runner's loop.

use std::cell::Cell;

use super::*;
use crate::commit::CommitRoute;
use crate::runner::verbs::tests::{job, runner, wire};

fn call<C: Containers>(
    r: &mut Runner<C>,
    verb: Verb,
    input: &[u8],
) -> std::io::Result<serde_json::Value> {
    let mut out = Vec::new();
    serve(r, verb, input, &mut out)?;
    let line = out.split(|b| *b == b'\n').next().unwrap_or_default();
    serde_json::from_slice(line).map_err(std::io::Error::other)
}

#[test]
fn each_verb_reads_its_payload_and_replies_one_json_line() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, _) = runner(dir.path());
    let (a, tar) = job("A", CommitRoute::Queue);
    let (b, tar_b) = job("B", CommitRoute::Queue);
    let (id, idb) = (a.id.clone(), b.id.clone());
    let committed = call(
        &mut r,
        Verb::Commit,
        &wire("t1", vec![(a, tar), (b, tar_b)], Vec::new()),
    )
    .unwrap();
    assert_eq!(committed["commit"], "applied");
    let ask = format!(r#"{{"jar_version":"J","ids":["{id}"],"keys":[],"tokens":["t1"]}}"#);
    let preflight = call(&mut r, Verb::Preflight, ask.as_bytes()).unwrap();
    assert_eq!(preflight["jobs"][&id]["state"], "running");
    let status = call(&mut r, Verb::Status, ask.as_bytes()).unwrap();
    assert_eq!(status["tokens"]["t1"]["commit"], "applied");
    let ls = call(&mut r, Verb::Ls, b"").unwrap();
    assert_eq!(ls.as_object().map(serde_json::Map::len), Some(2));
    let reorder = format!(r#"{{"id":"{idb}","run":"v1","to":"head"}}"#);
    assert_eq!(
        call(&mut r, Verb::Reorder, reorder.as_bytes()).unwrap(),
        false
    );
    let fetch = format!(r#"{{"id":"{id}","run":null}}"#);
    assert_eq!(
        call(&mut r, Verb::Fetch, fetch.as_bytes()).unwrap()["job"]["run"],
        "v1"
    );
    let cancel = format!(r#"{{"id":"{id}","run":null,"by":"ui"}}"#);
    assert_eq!(
        call(&mut r, Verb::Cancel, cancel.as_bytes()).unwrap()["state"],
        "stopping"
    );
    let bad = call(&mut r, Verb::Cancel, b"{}").unwrap_err();
    assert_eq!(bad.kind(), std::io::ErrorKind::InvalidData);
    call(&mut r, Verb::Commit, b"not json\n").unwrap_err();
}

/// A clock that only counts its waits.
struct Counting(Cell<u64>);

impl Clock for Counting {
    fn now(&self) -> u64 {
        self.0.get()
    }

    fn sleep(&self, _: u64) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn the_loop_runs_its_rounds_and_survives_a_failed_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, fake) = runner(dir.path());
    let clock = Counting(Cell::new(0));
    run_loop(&mut r, &clock, 5, Some(2));
    assert_eq!(clock.now(), 2);
    std::fs::remove_dir_all(dir.path()).unwrap();
    std::fs::write(dir.path(), "not a directory").unwrap();
    run_loop(&mut r, &clock, 5, Some(1));
    assert_eq!(clock.now(), 3);
    assert_eq!(fake.0.borrow().started, Vec::<String>::new());
}

#[test]
fn the_system_clock_reads_the_wall_time() {
    assert!(SystemClock.now() > 1_700_000_000);
    SystemClock.sleep(0);
}

/// A payload whose every read fails.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("broken pipe"))
    }
}

#[test]
fn a_payload_is_read_whole_or_fails_at_its_deadline() {
    let mut whole = String::new();
    Deadline::new(&b"a payload"[..], 5)
        .read_to_string(&mut whole)
        .unwrap();
    assert_eq!(whole, "a payload");
    // A sender that writes a little, then nothing more, its end left open.
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(b"{").unwrap();
    let mut late = Deadline::new(reader, 1);
    let mut got = Vec::new();
    let err = late.read_to_end(&mut got).unwrap_err();
    assert_eq!(
        (err.kind(), got.as_slice()),
        (std::io::ErrorKind::TimedOut, &b"{"[..])
    );
    assert!(err.to_string().contains("within 1 s"), "{err}");
    drop(writer);
    // A read that fails is the verb's error.
    let err = Deadline::new(Broken, 5).read(&mut [0; 4]).unwrap_err();
    assert_eq!(err.to_string(), "broken pipe");
}
