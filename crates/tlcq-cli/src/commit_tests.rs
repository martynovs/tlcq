//! Tests of the commit's wire format and the VM's checks of a job.

use super::*;
use crate::job::File;
use crate::runner::state::tests::info;

pub(crate) fn contents() -> Contents {
    Contents::new(
        vec![
            File {
                path: "model/M.tla".to_owned(),
                bytes: b"---- MODULE M ----\n====\n".to_vec(),
            },
            File {
                path: job::CFG.to_owned(),
                bytes: b"INIT Init\n".to_vec(),
            },
        ],
        Limits::default(),
    )
    .unwrap()
}

pub(crate) fn commit_job(contents: &Contents, jar: &str) -> (CommitJob, Vec<u8>) {
    let tar = contents.tarball().unwrap();
    let info = info();
    let id = crate::id::id(&crate::id::manifest(
        &info.spec,
        contents,
        &info.tlc_opts,
        &format!("tlc {jar}"),
    ));
    (
        CommitJob {
            id,
            info,
            route: CommitRoute::Queue,
            tar_len: tar.len() as u64,
        },
        tar,
    )
}

fn request(jobs: Vec<CommitJob>) -> CommitRequest {
    CommitRequest {
        versions: crate::engine::test_versions("J"),
        token: "t".to_owned(),
        pending: Vec::new(),
        jobs,
        supersede: Vec::new(),
    }
}

#[test]
fn a_commit_round_trips_on_the_wire() {
    let c = contents();
    let (a, tar_a) = commit_job(&c, "J");
    let (mut b, tar_b) = commit_job(&c, "K");
    b.route = CommitRoute::NowOrRefuse;
    let req = request(vec![a, b]);
    let wire = encode(&req, &[tar_a.clone(), tar_b.clone()]).unwrap();
    let (back, tars) = decode(wire.as_slice(), 1 << 20).unwrap();
    assert_eq!(back, req);
    assert_eq!(tars, [tar_a, tar_b]);
}

#[test]
fn a_short_or_oversized_stream_is_refused() {
    let c = contents();
    let (a, tar) = commit_job(&c, "J");
    let req = request(vec![a]);
    let wire = encode(&req, std::slice::from_ref(&tar)).unwrap();
    let cut = &wire[..wire.len() - 1];
    assert!(matches!(decode(cut, 1 << 20), Err(ReadError::Io(_))));
    let mut longer = wire.clone();
    longer.push(b'x');
    let err = decode(longer.as_slice(), 1 << 20).unwrap_err();
    assert!(
        err.to_string().contains("bytes after the last tarball"),
        "{err}"
    );
    assert!(matches!(
        decode(wire.as_slice(), 10),
        Err(ReadError::TooBig { .. })
    ));
    assert!(matches!(
        decode(&b"not json\n"[..], 10),
        Err(ReadError::Json(_))
    ));
}

#[test]
fn the_vm_recomputes_the_id_with_its_own_jars() {
    let c = contents();
    let (job, tar) = commit_job(&c, "J");
    assert_eq!(
        check_job(
            &job,
            &tar,
            &crate::engine::test_versions("J"),
            Limits::default()
        )
        .unwrap(),
        c
    );
    let err = check_job(
        &job,
        &tar,
        &crate::engine::test_versions("K"),
        Limits::default(),
    )
    .unwrap_err();
    assert!(err.contains("tlc versions or the files differ"), "{err}");
}

#[test]
fn the_vm_checks_a_tlx_job_with_its_own_tlx_rev() {
    let c = contents();
    let (mut job, tar) = commit_job(&c, "J");
    job.info.engine = crate::engine::Engine::Tlx;
    job.id = crate::id::id(&crate::id::manifest(
        &job.info.spec,
        &c,
        &job.info.tlc_opts,
        "tlx R",
    ));
    let vm = crate::engine::test_versions("J");
    check_job(&job, &tar, &vm, Limits::default()).unwrap();
    let stale = crate::engine::Versions {
        tlx_rev: Some("old".to_owned()),
        ..vm.clone()
    };
    let err = check_job(&job, &tar, &stale, Limits::default()).unwrap_err();
    assert!(err.contains("tlx versions or the files differ"), "{err}");
    let none = crate::engine::Versions {
        tlx_rev: None,
        ..vm
    };
    let err = check_job(&job, &tar, &none, Limits::default()).unwrap_err();
    assert!(err.contains("tlx is not installed on the VM"), "{err}");
}

#[test]
fn a_hostile_tarball_is_refused() {
    // Written by hand: the tar crate itself refuses to build a `..` path.
    let tar_of = |path: &str, kind: tar::EntryType| {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_old();
        header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &[][..]).unwrap();
        builder.into_inner().unwrap()
    };
    let bad = read_tarball(
        &tar_of("model/../x.tla", tar::EntryType::Regular),
        Limits::default(),
    );
    assert!(bad.unwrap_err().contains(".."));
    let link = read_tarball(
        &tar_of("model/l.tla", tar::EntryType::Symlink),
        Limits::default(),
    );
    assert!(link.unwrap_err().contains("not a regular file"));
    let dir = read_tarball(
        &tar_of("model/", tar::EntryType::Directory),
        Limits::default(),
    )
    .unwrap();
    assert_eq!(dir.files(), []);
    let two = contents().tarball().unwrap();
    let one = Limits {
        bytes: 1 << 20,
        entries: 1,
    };
    assert!(read_tarball(&two, one)
        .unwrap_err()
        .contains("over 1 files"));
    read_tarball(b"garbage that is not a tarball at all", Limits::default()).unwrap_err();
}

#[test]
fn a_moved_run_s_name_and_the_spec_must_be_plain_names() {
    let c = contents();
    let (mut job, tar) = commit_job(&c, "J");
    for bad in ["../..", "l0", "v1", "l", "l1/x"] {
        job.info.run = Some(bad.to_owned());
        let err = check_job(
            &job,
            &tar,
            &crate::engine::test_versions("J"),
            Limits::default(),
        )
        .unwrap_err();
        assert!(err.contains("not a laptop run's name"), "{bad}: {err}");
    }
    job.info.run = Some("l12".to_owned());
    check_job(
        &job,
        &tar,
        &crate::engine::test_versions("J"),
        Limits::default(),
    )
    .unwrap();
    job.info.spec = "../M".to_owned();
    let err = check_job(
        &job,
        &tar,
        &crate::engine::test_versions("J"),
        Limits::default(),
    )
    .unwrap_err();
    assert!(err.contains("not a module name"), "{err}");
}
