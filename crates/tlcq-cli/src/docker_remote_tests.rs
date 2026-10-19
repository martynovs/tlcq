//! Tests of the VM calls, with a script standing in for `docker`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;

/// A `docker` running `body` after logging its arguments and stdin.
fn remote(dir: &Path, body: &str, context: Option<&str>) -> DockerRemote {
    let program = dir.join("docker");
    let script = format!(
        "#!/bin/sh\necho \"$*\" > {args}\ncat > {stdin}\n{body}\n",
        args = dir.join("args").display(),
        stdin = dir.join("stdin").display()
    );
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    DockerRemote {
        program,
        context: context.map(str::to_owned),
        container: "tlcq-runner".to_owned(),
        deadline: Duration::from_secs(10),
        connect: Duration::from_secs(10),
    }
}

fn request() -> PreflightRequest {
    PreflightRequest {
        versions: crate::engine::test_versions("J"),
        ..PreflightRequest::default()
    }
}

fn fetch() -> FetchRequest {
    FetchRequest {
        id: "a".to_owned(),
        run: None,
    }
}

#[test]
fn a_call_sends_its_payload_and_parses_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let reply = r#"{"jars_match":true,"jobs":{},"tokens":{},"older":[]}"#;
    let mut r = remote(dir.path(), &format!("echo '{reply}'"), Some("pve"));
    assert!(r.preflight(&request()).unwrap().jars_match);
    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(
        args,
        "--context pve exec -i tlcq-runner tlcq vm preflight\n"
    );
    let sent = std::fs::read_to_string(dir.path().join("stdin")).unwrap();
    assert!(sent.contains(r#""jar_version":"J""#));
    let mut local = remote(dir.path(), r#"echo '{"commit":"not_applied"}'"#, None);
    assert_eq!(local.commit(b"wire").unwrap(), CommitReply::NotApplied);
    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert_eq!(args, "exec -i tlcq-runner tlcq vm commit\n");
}

#[test]
fn a_call_that_never_reached_the_runner_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let mut down = remote(
        dir.path(),
        "echo 'error during connect: ssh: no route' >&2; exit 1",
        None,
    );
    assert!(matches!(
        down.preflight(&request()),
        Err(CallError::Unreachable(_))
    ));
    let mut gone = remote(dir.path(), "", None);
    gone.program = dir.path().join("missing");
    assert!(matches!(
        gone.preflight(&request()),
        Err(CallError::Unreachable(_))
    ));
}

#[test]
fn any_other_failure_is_an_unknown_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let mut failed = remote(
        dir.path(),
        "echo 'error: the verb: disk full' >&2; exit 1",
        None,
    );
    assert_eq!(
        failed.commit(b"wire"),
        Err(CallError::Failed("error: the verb: disk full".to_owned()))
    );
    let mut garbled = remote(dir.path(), "echo not-json", None);
    assert!(
        matches!(garbled.commit(b"wire"), Err(CallError::Failed(e)) if e.starts_with("the reply"))
    );
    let mut slow = remote(dir.path(), "exec sleep 5", None);
    slow.deadline = Duration::from_millis(200);
    assert!(matches!(slow.commit(b"wire"), Err(CallError::Failed(e)) if e.starts_with("no reply")));
}

#[test]
fn a_read_with_no_answer_by_the_connect_timeout_is_unreachable_a_change_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut silent = remote(dir.path(), "exec sleep 5", None);
    silent.connect = Duration::from_millis(200);
    for read in [
        silent.preflight(&request()).map(|_| ()),
        silent.status(&request()).map(|_| ()),
        silent.ls().map(|_| ()),
        silent.fetch(&fetch()).map(|_| ()),
    ] {
        assert!(matches!(read, Err(CallError::Unreachable(e)) if e.starts_with("no answer")));
    }
    silent.deadline = Duration::from_millis(400);
    assert!(matches!(silent.commit(b"wire"), Err(CallError::Failed(_))));
    assert!(matches!(
        silent.cancel(&CancelRequest {
            id: "a".to_owned(),
            run: None,
            by: "w1".to_owned()
        }),
        Err(CallError::Failed(_))
    ));
    // A reply that has begun is not cut short by it. (Wide margins: macOS
    // scans a new script before its first run.)
    let mut slow = remote(dir.path(), "printf '['; sleep 2; echo ']'", None);
    slow.connect = Duration::from_secs(1);
    assert_eq!(slow.fetch(&fetch()).unwrap(), b"[]\n");
}

#[test]
fn podman_s_wording_of_a_missing_or_stopped_runner_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    for err in [
        r#"Error response from daemon: no container with name or ID "tlcq-runner" found: no such container"#,
        "Error: can only create exec sessions on running containers: container state improper",
    ] {
        let mut r = remote(
            dir.path(),
            &format!("echo '{err}' >&2; exit 125"),
            Some("pve"),
        );
        assert!(
            matches!(r.preflight(&request()), Err(CallError::Unreachable(_))),
            "{err}"
        );
    }
}

#[test]
fn the_runner_s_own_failure_is_failed_whatever_its_text() {
    let dir = tempfile::tempdir().unwrap();
    let err = "error: tlcq vm: the verb: docker ps: Cannot connect to the Docker daemon at unix:///run/podman/podman.sock";
    let mut r = remote(
        dir.path(),
        &format!("echo '{err}' >&2; exit 2"),
        Some("pve"),
    );
    assert!(matches!(r.preflight(&request()), Err(CallError::Failed(_))));
}
