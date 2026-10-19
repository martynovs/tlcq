//! End-to-end tests: run the built binary and check its behavior.
//!
//! This is what covers `main` for the coverage floor — `cargo llvm-cov`
//! captures coverage from the spawned subprocess. `CARGO_BIN_EXE_<name>` is
//! set by cargo for integration tests; no `assert_cmd` needed.

#![expect(
    clippy::unwrap_used,
    reason = "a failed setup step should fail the test where it happens"
)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_tlcq");

/// A state directory, two stand-in jars, a stand-in tlx with its rev, and a
/// `docker` that lists nothing. The laptop and the VM share the jars and tlx.
struct Vm {
    dir: tempfile::TempDir,
}

impl Vm {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let jars = dir.path().join("jars");
        std::fs::create_dir_all(&jars).unwrap();
        for jar in ["tla2tools.jar", "CommunityModules-deps.jar"] {
            std::fs::write(jars.join(jar), jar).unwrap();
        }
        // Plenty of memory, whatever the machine running the tests has.
        std::fs::write(
            dir.path().join("meminfo"),
            "MemTotal: 32768000 kB\nMemAvailable: 32768000 kB\n",
        )
        .unwrap();
        let docker = dir.path().join("docker");
        std::fs::write(&docker, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tlx = dir.path().join("tlx");
        std::fs::create_dir_all(&tlx).unwrap();
        std::fs::write(tlx.join("tlx"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(tlx.join("tlx"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(tlx.join("rev"), "R\n").unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The settings flags, `--memory-mb` aside.
    fn flags(&self) -> Vec<String> {
        let p = |n: &str| self.path(n).display().to_string();
        [
            "--state",
            &p("state"),
            "--jars",
            &p("jars"),
            "--tlx",
            &p("tlx/tlx"),
            "--docker",
            &p("docker"),
            "--e-cpus",
            "0-1",
            "--p-cpus",
            "2-3",
            "--meminfo",
            &p("meminfo"),
            "--disk-floor-gb",
            "0",
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

fn tlcq(args: &[String], stdin: &[u8]) -> Output {
    tlcq_in(Path::new("."), args, stdin)
}

fn tlcq_in(dir: &Path, args: &[String], stdin: &[u8]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.current_dir(dir);
    // Only the runner's settings: clearing everything would drop the coverage
    // run's profile path, and with it this binary's coverage.
    for var in [
        "STATE",
        "JARS",
        "JAVA",
        "TLX",
        "TLX_MEM_PERCENT",
        "DOCKER",
        "IMAGE",
        "VOLUME",
        "E_CPUS",
        "P_CPUS",
        "MEMORY_MB",
        "MEMINFO",
        "SLOTS",
        "WORKERS",
        "HEAP_MB",
        "OFFHEAP_MB",
        "CAP_SECS",
        "POLL_SECS",
        "DISK_FLOOR_GB",
    ] {
        cmd.env_remove(format!("TLCQ_{var}"));
    }
    // No config file: the machine's own would change the settings under test.
    cmd.env("TLCQ_CONFIG", "/dev/null");
    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the binary");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

fn args(head: &[&str], vm: &Vm, tail: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = head.iter().map(|s| (*s).to_owned()).collect();
    a.extend(vm.flags());
    a.extend(["--memory-mb".to_owned(), "32000".to_owned()]);
    a.extend(tail.iter().map(|s| (*s).to_owned()));
    a
}

#[test]
fn a_verb_reads_stdin_and_replies_on_stdout() {
    let vm = Vm::new();
    let out = tlcq(&args(&["vm"], &vm, &["ls"]), b"");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A space first, as soon as the verb starts: the laptop's sign it reached the runner.
    assert_eq!(String::from_utf8_lossy(&out.stdout), " {}\n");
    let bad = tlcq(&args(&["vm"], &vm, &["cancel"]), b"{}");
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("the verb"));
}

#[test]
fn the_runner_runs_its_rounds() {
    let vm = Vm::new();
    let out = tlcq(
        &args(&["runner", "--poll-secs", "0", "--rounds", "2"], &vm, &[]),
        b"",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(Path::new(&vm.path("state/admission.lock")).exists());
}

#[test]
fn a_runner_given_sigterm_marks_itself_stopping_and_its_next_start_clears_it() {
    let vm = Vm::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tlcq"))
        .args(args(&["runner", "--poll-secs", "60"], &vm, &[]))
        .env("TLCQ_CONFIG", "/dev/null")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let lock = vm.path("state/admission.lock");
    let start = std::time::Instant::now();
    while !Path::new(&lock).exists() {
        assert!(start.elapsed().as_secs() < 30, "the runner never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let term = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(term.success());
    assert!(child.wait().unwrap().success());
    assert!(Path::new(&vm.path("state/stopping")).exists());
    let out = tlcq(
        &args(&["runner", "--poll-secs", "0", "--rounds", "1"], &vm, &[]),
        b"",
    );
    assert!(out.status.success());
    assert!(!Path::new(&vm.path("state/stopping")).exists());
}

#[test]
fn an_unknown_attempt_fails_its_supervisor() {
    let vm = Vm::new();
    let out = tlcq(&args(&["exec"], &vm, &["a", "v1", "1"]), b"");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("attempt a v1 1"));
}

#[test]
fn missing_jars_or_memory_are_errors() {
    let vm = Vm::new();
    std::fs::remove_file(vm.path("jars/tla2tools.jar")).unwrap();
    let out = tlcq(&args(&["vm"], &vm, &["ls"]), b"");
    assert!(String::from_utf8_lossy(&out.stderr).contains("the jars in"));
    // No `--memory-mb` and no meminfo to read it from.
    std::fs::remove_file(vm.path("meminfo")).unwrap();
    let mut no_memory = vec!["vm".to_owned()];
    no_memory.extend(vm.flags());
    no_memory.push("ls".to_owned());
    let out = tlcq(&no_memory, b"");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--memory-mb"));
}

#[test]
fn help_lists_the_commands() {
    let out = tlcq(&["--help".to_owned()], b"");
    let help = String::from_utf8_lossy(&out.stdout);
    for cmd in ["runner", "exec", "vm"] {
        assert!(help.contains(cmd), "{help}");
    }
}

/// A git worktree holding a model directory, and a laptop whose `docker`
/// forwards each VM call to this binary's `vm` verbs on a local state directory.
/// Its laptop runs nothing (no slot), unless `runs` gives it a slot.
struct Laptop {
    vm: Vm,
    settings: Vec<String>,
}

impl Laptop {
    fn new(docker_body: Option<&str>) -> Self {
        let vm = Vm::new();
        let model = vm.path("repo/tla/counter");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(
            model.join("Counter.tla"),
            "---- MODULE Counter ----\n====\n",
        )
        .unwrap();
        std::fs::write(model.join("CT.cfg"), "INIT Init\n").unwrap();
        std::fs::write(model.join("CT_max.cfg"), "INIT Init\nNEXT Next\n").unwrap();
        std::fs::write(model.join("tlcq.toml"), "spec = \"Counter\"\n").unwrap();
        let git = Command::new("git")
            .arg("-C")
            .arg(vm.path("repo"))
            .args(["init", "--quiet"])
            .status();
        assert!(git.unwrap().success());
        let forward = format!(
            "while [ \"$1\" != vm ]; do shift; done\nexec {BIN} vm \"$2\" {}  --memory-mb 32000",
            vm.flags().join(" ")
        );
        let laptop_docker = vm.path("laptop-docker");
        std::fs::write(
            &laptop_docker,
            format!("#!/bin/sh\n{}\n", docker_body.unwrap_or(&forward)),
        )
        .unwrap();
        std::fs::set_permissions(&laptop_docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            vm,
            settings: vec!["--slots".to_owned(), "0".to_owned()],
        }
    }

    /// This laptop with one slot, a cap of `cap` seconds, and `java` and tlx
    /// a script that runs `body`, standing in for either engine.
    fn runs(mut self, body: &str, cap: u64) -> Self {
        let laptop = &mut self;
        let java = laptop.vm.path("java");
        let script = format!(
            "#!/bin/sh
echo $$ > {}
{body}
",
            laptop.vm.path("java.pid").display()
        );
        for engine in [java.clone(), laptop.vm.path("tlx/tlx")] {
            std::fs::write(&engine, &script).unwrap();
            std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        laptop.settings = [
            "--slots",
            "1",
            "--java",
            &java.display().to_string(),
            "--cap-secs",
            &cap.to_string(),
            "--poll-secs",
            "1",
            "--disk-floor-gb",
            "0",
        ]
        .map(str::to_owned)
        .to_vec();
        self
    }

    /// Poll `status` until its output has `want`, for up to 30 seconds.
    fn status_until(&self, want: &str) -> Output {
        let mut out = self.status(&[]);
        for _ in 0..60 {
            if text(&out).contains(want) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            out = self.status(&[]);
        }
        assert!(text(&out).contains(want), "no {want:?} in {}", text(&out));
        out
    }

    fn status(&self, ids: &[&str]) -> Output {
        self.command("status", ids)
    }

    /// Run the agent command `cmd` with `ids` in the worktree.
    fn command(&self, cmd: &str, ids: &[&str]) -> Output {
        let p = |n: &str| self.vm.path(n).display().to_string();
        let mut a: Vec<String> = [
            cmd,
            "--state",
            &p("laptop"),
            "--jars",
            &p("jars"),
            "--tlx",
            &p("tlx/tlx"),
            "--docker",
            &p("laptop-docker"),
        ]
        .map(str::to_owned)
        .to_vec();
        a.extend(self.settings.iter().cloned());
        a.extend(ids.iter().map(|s| (*s).to_owned()));
        tlcq_in(&self.vm.path("repo"), &a, b"")
    }

    fn submit(&self, extra: &[&str], stdin: &[u8]) -> Output {
        let p = |n: &str| self.vm.path(n).display().to_string();
        let mut a: Vec<String> = [
            "submit",
            "--state",
            &p("laptop"),
            "--jars",
            &p("jars"),
            "--tlx",
            &p("tlx/tlx"),
            "--docker",
            &p("laptop-docker"),
        ]
        .map(str::to_owned)
        .to_vec();
        a.extend(self.settings.iter().cloned());
        a.push(p("repo/tla/counter"));
        a.extend(extra.iter().map(|s| (*s).to_owned()));
        tlcq(&a, stdin)
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn submit_places_jobs_on_the_vm_and_reports_them() {
    let laptop = Laptop::new(None);
    let out = laptop.submit(&[], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let report = text(&out);
    assert!(
        report.contains("CT.cfg") && report.contains("vm v1 started"),
        "{report}"
    );
    assert!(
        report.contains("CT_max.cfg") && report.contains("skipped as max"),
        "{report}"
    );
    let named = laptop.submit(
        &["CT_max.cfg", "--slow", "--config", "-"],
        b"replace = true\n",
    );
    assert_eq!(named.status.code(), Some(0), "{}", text(&named));
    assert!(
        text(&named).contains("vm v1 started in P  (tlx, long)"),
        "{}",
        text(&named)
    );
}

#[test]
fn submit_without_a_reachable_vm_or_with_bad_input_exits_2_or_3() {
    let down = Laptop::new(Some(
        "echo 'Cannot connect to the Docker daemon' >&2; exit 1",
    ))
    .runs("exit 0", 60);
    // A memory request is for VM runs: refused on the laptop, and not a size at all.
    let local = down.submit(&["--local", "--memory", "64G"], b"");
    assert_eq!(local.status.code(), Some(2), "{}", text(&local));
    assert!(
        text(&local).contains("--memory is for VM runs"),
        "{}",
        text(&local)
    );
    let bad = down.submit(&["--memory", "lots"], b"");
    assert_eq!(bad.status.code(), Some(2), "{}", text(&bad));
    // Offline, the unknown job runs on the laptop.
    let out = down.submit(&[], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("laptop l1"), "{}", text(&out));
    let ended = down.status_until("ok, ");
    assert!(text(&ended).contains("unreachable"), "{}", text(&ended));
    let missing = down.submit(&["--config", "/no/such/file"], b"");
    assert!(text(&missing).contains("reading /no/such/file"));
    let failing = Laptop::new(Some("echo boom >&2; exit 1"));
    let out = failing.submit(&[], b"");
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    let cancel = failing.command("cancel", &["x"]);
    assert_eq!(cancel.status.code(), Some(3), "{}", text(&cancel));
}

#[test]
fn status_reports_the_agent_s_jobs_and_named_ones() {
    let laptop = Laptop::new(None);
    assert_eq!(laptop.submit(&[], b"").status.code(), Some(0));
    let out = laptop.status(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        text(&out).contains("CT.cfg") && text(&out).contains("v1"),
        "{}",
        text(&out)
    );
    let unknown = laptop.status(&["nope"]);
    assert_eq!(unknown.status.code(), Some(3), "{}", text(&unknown));
    let outside = tlcq_in(Path::new("/"), &["status".to_owned()], b"");
    assert_eq!(outside.status.code(), Some(2));
}

#[test]
fn ls_cancel_and_fetch_act_on_the_vm_s_jobs() {
    let laptop = Laptop::new(None);
    assert_eq!(laptop.submit(&[], b"").status.code(), Some(0));
    let ls = laptop.command("ls", &[]);
    assert_eq!(ls.status.code(), Some(0), "{}", text(&ls));
    let entry = std::fs::read_dir(laptop.vm.path("laptop/jobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let id = entry.file_name().to_string_lossy().into_owned();
    assert!(text(&ls).contains(id.get(..12).unwrap()), "{}", text(&ls));
    // This runner's containers never run, so its attempt reads as undone:
    // queued, with no files yet.
    let fetched = laptop.command("fetch", &[&id]);
    assert_eq!(fetched.status.code(), Some(0), "{}", text(&fetched));
    assert!(
        text(&fetched).contains("queued on the vm"),
        "{}",
        text(&fetched)
    );
    // Queued, not paused: a resume is refused, as is a memory that is no size.
    let resumed = laptop.command("resume", &[&id, "--memory", "4G"]);
    assert_eq!(resumed.status.code(), Some(2), "{}", text(&resumed));
    assert!(
        text(&resumed).contains("it is not paused on the VM"),
        "{}",
        text(&resumed)
    );
    let bad = laptop.command("resume", &[&id, "--memory", "lots"]);
    assert_eq!(bad.status.code(), Some(2), "{}", text(&bad));
    let cancelled = laptop.command("cancel", &[&id]);
    assert_eq!(cancelled.status.code(), Some(0), "{}", text(&cancelled));
    assert!(
        text(&cancelled).contains("cancelled by"),
        "{}",
        text(&cancelled)
    );
    let refused = laptop.command("cancel", &["nope"]);
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused));
}

#[test]
fn an_agent_command_with_bad_settings_exits_2() {
    let laptop = Laptop::new(None);
    let p = |n: &str| laptop.vm.path(n).display().to_string();
    let no_jars = ["status", "--state", &p("laptop"), "--jars", &p("nowhere")].map(str::to_owned);
    let out = tlcq_in(&laptop.vm.path("repo"), &no_jars, b"");
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("the jars in"), "{}", text(&out));
    let bad_state = ["ls", "--state", &p("jars/tla2tools.jar")].map(str::to_owned);
    let out = tlcq_in(&laptop.vm.path("repo"), &bad_state, b"");
    assert!(text(&out).contains("the state directory"), "{}", text(&out));
}

#[test]
fn without_home_or_xdg_the_directories_must_be_given() {
    let laptop = Laptop::new(None);
    let run = |args: &[&str]| {
        let mut cmd = Command::new(BIN);
        for var in [
            "HOME",
            "XDG_STATE_HOME",
            "XDG_DATA_HOME",
            "TLCQ_STATE",
            "TLCQ_JARS",
            "TLCQ_CONFIG",
        ] {
            cmd.env_remove(var);
        }
        cmd.current_dir(laptop.vm.path("repo"))
            .args(args)
            .output()
            .unwrap()
    };
    let no_state = run(&["ls"]);
    assert!(
        text(&no_state).contains("$HOME is not set"),
        "{}",
        text(&no_state)
    );
    let state = laptop.vm.path("laptop").display().to_string();
    let no_jars = run(&["status", "--state", &state]);
    assert!(
        text(&no_jars).contains("$HOME is not set"),
        "{}",
        text(&no_jars)
    );
}

/// The id of the one job a laptop placed.
fn only_id(laptop: &Laptop) -> String {
    let entry = std::fs::read_dir(laptop.vm.path("laptop/jobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    entry.file_name().to_string_lossy().into_owned()
}

#[test]
fn a_laptop_run_ends_with_tlc_s_verdict_and_its_files_are_fetched() {
    let laptop = Laptop::new(None).runs(
        "echo 'Progress(2) at 2026-10-18 10:00:00: 30 states generated, 20 distinct states found, 0 states left on queue.'\nexit 0",
        60,
    );
    let out = laptop.submit(&[], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("laptop l1"), "{}", text(&out));
    let ended = laptop.status_until("on laptop");
    assert!(
        text(&ended).contains("CT.cfg") && text(&ended).contains("ok, 20 distinct states"),
        "{}",
        text(&ended)
    );
    assert_eq!(ended.status.code(), Some(0));
    let id = only_id(&laptop);
    let fetched = laptop.command("fetch", &[&id]);
    assert_eq!(fetched.status.code(), Some(0), "{}", text(&fetched));
    let files = laptop.vm.path("repo/tmp/tlcq").join(&id).join("l1/0");
    assert!(files.join("tlc.log").exists() && files.join("result.json").exists());
    let ls = laptop.command("ls", &[]);
    assert!(
        text(&ls).contains("ok, 20 distinct states"),
        "{}",
        text(&ls)
    );
}

#[test]
fn a_laptop_cancel_stops_tlc_before_it_returns() {
    let laptop = Laptop::new(None).runs("exec sleep 60", 60);
    assert_eq!(laptop.submit(&[], b"").status.code(), Some(0));
    laptop.status_until("running on the laptop");
    while !laptop.vm.path("java.pid").exists() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let id = only_id(&laptop);
    let cancelled = laptop.command("cancel", &[&id]);
    assert_eq!(cancelled.status.code(), Some(0), "{}", text(&cancelled));
    assert!(
        text(&cancelled).contains("cancelled by"),
        "{}",
        text(&cancelled)
    );
    let pid = std::fs::read_to_string(laptop.vm.path("java.pid")).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .status()
        .unwrap();
    assert!(!alive.success(), "TLC still runs");
    let status = laptop.status(&[]);
    assert_eq!(status.status.code(), Some(3), "{}", text(&status));
}

#[test]
fn a_laptop_run_past_its_cap_times_out_or_moves_to_the_vm() {
    let laptop = Laptop::new(None).runs("exec sleep 60", 2);
    assert_eq!(laptop.submit(&[], b"").status.code(), Some(0));
    let timed_out = laptop.status_until("timeout");
    assert_eq!(timed_out.status.code(), Some(3), "{}", text(&timed_out));
    let moved = Laptop::new(None).runs("exec sleep 60", 2);
    let out = moved.submit(&["--vm-on-timeout"], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    // On the VM under its laptop run's name (where this runner's containers,
    // which never run, end it `never started` after a few polls).
    let on_vm = moved.status_until(" vm");
    assert!(text(&on_vm).contains("l1"), "{}", text(&on_vm));
}

#[test]
fn the_runner_serves_its_page() {
    use std::io::{BufRead, Read};
    let vm = Vm::new();
    let site = tempfile::tempdir().unwrap();
    std::fs::write(site.path().join("index.html"), "<title>tlcq</title>").unwrap();
    let mut child = Command::new(BIN)
        .args(args(
            &[
                "runner",
                "--poll-secs",
                "1",
                "--rounds",
                "4",
                "--ui",
                "127.0.0.1:0",
                "--ui-dir",
                site.path().to_str().unwrap(),
            ],
            &vm,
            &[],
        ))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let addr = line.trim().rsplit("http://").next().unwrap().to_owned();
    let get = |path: &str| {
        let mut s = std::net::TcpStream::connect(&addr).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    };
    assert!(get("/").contains("<title>tlcq</title>"), "{line}");
    assert!(get("/api/ended").contains("\"total\":0"), "{line}");
    let state = get("/api/state");
    assert!(
        state.starts_with("HTTP/1.1 200") && state.contains("\"slots\""),
        "{state}"
    );
    // It ends after its rounds, the page with it.
    assert!(child.wait().unwrap().success());
    // An address another process holds: the runner says so and stops.
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = held.local_addr().unwrap().to_string();
    let out = tlcq(
        &args(&["runner", "--rounds", "1", "--ui", &taken], &vm, &[]),
        b"",
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).contains("the page's address"), "{}", text(&out));
}

#[test]
fn a_config_file_fills_in_unset_settings_and_refuses_unknown_keys() {
    let vm = Vm::new();
    let config = vm.path("config.toml");
    let state = vm.path("from-config");
    // A VM that is down: `ls` lists the laptop's side and exits 0.
    let down = vm.path("down-docker");
    std::fs::write(
        &down,
        "#!/bin/sh\necho 'Cannot connect to the Docker daemon' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&down, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |text: &str, env_state: Option<&str>| {
        std::fs::write(&config, text).unwrap();
        let mut cmd = Command::new(BIN);
        cmd.env("TLCQ_CONFIG", &config)
            .env_remove("TLCQ_STATE")
            .env("TLCQ_DOCKER", &down);
        if let Some(s) = env_state {
            cmd.env("TLCQ_STATE", s);
        }
        cmd.arg("ls").output().unwrap()
    };
    let out = run(&format!("state = \"{}\"\n", state.display()), None);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(state.join("jobs").exists());
    // A variable beats the file.
    let other = vm.path("from-env");
    let out = run(
        &format!("state = \"{}\"\n", state.display()),
        Some(&other.display().to_string()),
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(other.join("jobs").exists());
    let bad = run("no_such_thing = 1\n", None);
    assert_eq!(bad.status.code(), Some(2));
    assert!(
        text(&bad).contains("no_such_thing is no setting"),
        "{}",
        text(&bad)
    );
    std::fs::remove_file(&config).unwrap();
    let missing = run_missing(&config);
    assert!(text(&missing).contains("reading"), "{}", text(&missing));
}

/// A command whose explicit config file is missing.
fn run_missing(config: &Path) -> Output {
    Command::new(BIN)
        .env("TLCQ_CONFIG", config)
        .arg("ls")
        .output()
        .unwrap()
}

/// D14 leaves this out of the model: a model directory holding a name the VM
/// refuses. `submit` stops at its input checks: nothing superseded, nothing written.
#[test]
fn a_name_the_vm_refuses_stops_submit_before_anything_is_written() {
    let laptop = Laptop::new(None);
    assert_eq!(laptop.submit(&[], b"").status.code(), Some(0));
    let listing = |l: &Laptop| {
        let mut names: Vec<String> = ["laptop/jobs", "laptop/follows"]
            .iter()
            .flat_map(|d| std::fs::read_dir(l.vm.path(d)).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = listing(&laptop);
    let model = laptop.vm.path("repo/tla/counter");
    std::fs::write(model.join("CT.cfg"), "INIT Init\nNEXT Next\n").unwrap();
    std::fs::write(
        model.join("Counter copy.tla"),
        "---- MODULE Counter ----\n====\n",
    )
    .unwrap();
    let out = laptop.submit(&["--replace"], b"");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(text(&out).contains("Counter copy.tla"), "{}", text(&out));
    assert_eq!(listing(&laptop), before);
    // The earlier version still runs on the VM: nothing superseded it.
    let status = laptop.status(&[]);
    let report = text(&status);
    assert!(
        report.contains("running on vm") && !report.contains("replaced"),
        "{report}"
    );
}

#[test]
fn a_verb_whose_payload_never_ends_stops_at_its_deadline() {
    let vm = Vm::new();
    let mut child = Command::new(BIN)
        .args(args(&["vm"], &vm, &["--deadline-secs", "1", "status"]))
        .env("TLCQ_CONFIG", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The sender writes a little and then goes quiet, its stdin left open.
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"{").unwrap();
    let out = child.wait_with_output().unwrap();
    drop(stdin);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out).contains("did not arrive within 1 s"),
        "{}",
        text(&out)
    );
}

/// A laptop whose engines record which of them ran, and with what, in
/// `ran`, then pass.
fn recording() -> Laptop {
    let laptop = Laptop::new(None);
    let ran = laptop.vm.path("ran").display().to_string();
    laptop.runs(
        &format!(
            "printf '%s\\n' \"$0\" \"$@\" > {ran}\necho 'Progress(2) at 2026-10-18 10:00:00: 30 states generated, 20 distinct states found, 0 states left on queue.'\nexit 0"
        ),
        60,
    )
}

#[test]
fn a_default_submit_runs_tlx_and_engine_tlc_runs_java() {
    let laptop = recording();
    let out = laptop.submit(&["CT.cfg"], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("(tlx, "), "{}", text(&out));
    laptop.status_until("ok, 20 distinct states");
    let ran = std::fs::read_to_string(laptop.vm.path("ran")).unwrap();
    let mut lines = ran.lines();
    assert!(lines.next().unwrap().ends_with("tlx/tlx"), "{ran}");
    assert_eq!(lines.next(), Some("check"), "{ran}");
    for want in ["-maxmem\n8192M\n", "-checkpoint\n10\n", "-workers\n8\n"] {
        assert!(ran.contains(want), "{want:?} not in {ran}");
    }
    assert!(!ran.contains("-Xmx") && !ran.contains("tlc2.TLC"), "{ran}");
    // On TLC: a new id, run by java.
    let out = laptop.submit(&["CT.cfg", "--engine", "tlc"], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(text(&out).contains("(tlc, "), "{}", text(&out));
    let start = std::time::Instant::now();
    loop {
        let ran = std::fs::read_to_string(laptop.vm.path("ran")).unwrap_or_default();
        if ran.lines().next().is_some_and(|l| l.ends_with("/java")) {
            assert!(ran.contains("tlc2.TLC"), "{ran}");
            break;
        }
        assert!(start.elapsed().as_secs() < 30, "java never ran: {ran}");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn a_temporal_must_fail_on_tlx_gets_the_hint() {
    let laptop = Laptop::new(None);
    std::fs::write(
        laptop.vm.path("repo/tla/counter/CT_live.cfg"),
        "\\* must fail Live\nINIT Init\nNEXT Next\nPROPERTY Live\n",
    )
    .unwrap();
    let out = laptop.submit(&["CT_live.cfg"], b"");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        text(&out).contains(
            "hint: CT_live.cfg must fail a temporal property; tlx checks liveness after the full search, --engine tlc --replace stops at the violation"
        ),
        "{}",
        text(&out)
    );
    let tlc = laptop.submit(&["CT_live.cfg", "--engine", "tlc", "--replace"], b"");
    assert_eq!(tlc.status.code(), Some(0), "{}", text(&tlc));
    assert!(!text(&tlc).contains("hint:"), "{}", text(&tlc));
}

#[test]
fn a_laptop_without_tlx_refuses_only_tlx_jobs() {
    let laptop = Laptop::new(None);
    std::fs::remove_file(laptop.vm.path("tlx/rev")).unwrap();
    let out = laptop.submit(&["CT.cfg"], b"");
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    assert!(
        text(&out).contains("CT.cfg: tlx is not installed (run `just install`)"),
        "{}",
        text(&out)
    );
    let tlc = laptop.submit(&["CT.cfg", "--engine", "tlc"], b"");
    assert_eq!(tlc.status.code(), Some(0), "{}", text(&tlc));
    assert!(text(&tlc).contains("vm v1 started"), "{}", text(&tlc));
}
