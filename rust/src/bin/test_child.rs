use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use opencodeserver::platform::{
    close_fd, ignore_signal, join_process_group, parent_process_group, parent_process_id,
    process_exists, process_snapshot,
};
use opencodeserver::test_events;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Behavior knobs for this fixture are marker files placed next to the
/// fixture executable by the test that owns that copy. Each test copies the
/// binary into its own directory, so knobs are per-test instances and never
/// touch the shared process environment (parallel tests stay isolated).
const HOLD_ENDPOINT_MARKER: &str = "hold-endpoint";
const HANG_ON_VERSION_MARKER: &str = "hang-on-version";
const HANG_PID_LOG: &str = "hang-on-version.pids";
const JOIN_PARENT_GROUP_MARKER: &str = "join-parent-process-group";
/// File containing a target PGID: `serve` joins that real process group
/// (modeling a child that abandons its constructed group for a foreign
/// one that contains an unrelated sentinel).
const JOIN_GROUP_OF_MARKER: &str = "join-group-of.pgid";
/// Written by `serve` right after the join-group-of move succeeds, so a
/// test probe can wait for the observable group change before failing
/// identity confirmation.
const JOINED_GROUP_READY_MARKER: &str = "joined-group.ready";
const GROUP_ESCAPE_HOLD_MARKER: &str = "group-escape.hold";
const GROUP_ESCAPE_RELEASE_MARKER: &str = "group-escape.release";
/// `serve` answers every health request with `{"healthy":false,...}`,
/// modeling an OpenCode whose endpoint rejects the health check.
const UNHEALTHY_HEALTH_MARKER: &str = "unhealthy-health";
/// File containing a target PGID: `serve` joins that foreign process group
/// on the FIRST accepted connection, immediately before serving the health
/// response — modeling a process that escapes its dedicated group between
/// the supervisor's initial identity inspection and its post-health
/// re-inspection.
const ESCAPE_ON_ACCEPT_PGID_MARKER: &str = "escape-on-accept.pgid";
const IGNORE_SIGTERM_MARKER: &str = "ignore-sigterm";
/// Written by `serve` immediately after `ignore_signal(SIGTERM)` takes
/// effect, so a test probe can establish a happens-before: the child has
/// provably ignored SIGTERM before the supervisor's grace signal is sent.
const IGNORE_SIGTERM_READY_MARKER: &str = "ignore-sigterm.ready";
const HOLD_VERSION_STDOUT_MARKER: &str = "hold-version-stdout";
/// `--version` spawns a group descendant that closes stdout and remains
/// silent; the query must inspect the group before reaping the leader.
const SILENT_VERSION_DESCENDANT_MARKER: &str = "silent-version-descendant";
const VERSION_OUTPUT_DESCENDANT_MARKER: &str = "version-output-descendant";
const FAST_EXIT_VERSION_DESCENDANT_MARKER: &str = "fast-exit-version-descendant";
/// One-shot `serve` gate: after the leader responds to health with its
/// same-group descendant alive, it stays running while this marker exists.
/// Tests remove it only after the supervisor has observed Healthy, ensuring
/// the identity recheck cannot race the modeled leader exit.
const LEADER_EXIT_DESCENDANT_MARKER: &str = "leader-exit-descendant";
const LEADER_EXIT_DESCENDANT_PID_LOG: &str = "leader-exit-descendant.pids";
const LEADER_EXIT_DESCENDANT_READY: &str = "leader-exit-descendant.ready";
/// `--version` prints a version, writes its PID, closes its stdout, and
/// keeps running: models a direct child that stops writing while alive.
const CLOSE_VERSION_STDOUT_MARKER: &str = "close-version-stdout-then-live";
/// `--version` floods stdout well past the query's 4096-byte bound without
/// closing the pipe.
const FLOOD_VERSION_STDOUT_MARKER: &str = "flood-version-stdout";
/// `--version` prints output containing a control character.
const INVALID_VERSION_OUTPUT_MARKER: &str = "invalid-version-output";
const PRE_EXEC_GATE_MARKER: &str = "pre-exec-gate";
const PRE_EXEC_RELEASE_MARKER: &str = "pre-exec.release";
const PORT_RESERVATION_HELD_MARKER: &str = "port-reservation-held";
const PORT_RESERVATION_RELEASE_MARKER: &str = "port-reservation.release";
const PORT_BIND_READY_MARKER: &str = "port-bind.ready";

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "wait".to_owned());
    match mode.as_str() {
        "--version" => version(),
        "serve" => serve(),
        "wait" => loop {
            thread::sleep(Duration::from_secs(1));
        },
        "ignore" => {
            ignore_signal(libc::SIGTERM).expect("ignore SIGTERM");
            if marker_present(LEADER_EXIT_DESCENDANT_MARKER) {
                fs::write(marker_path(LEADER_EXIT_DESCENDANT_READY), b"ready\n")
                    .expect("write leader-exit descendant ready marker");
            }
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        "tree" => {
            let executable = std::env::current_exe().expect("current executable");
            let mut child = Command::new(executable)
                .arg("wait")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn descendant");
            println!("{}", child.id());
            let status = child.wait().expect("wait for descendant");
            std::process::exit(status.code().unwrap_or(1));
        }
        "hold" => hold(),
        other => {
            eprintln!("unknown fixture mode: {other}");
            std::process::exit(2);
        }
    }
}

fn marker_present(name: &str) -> bool {
    marker_path(name).is_file()
}

fn marker_pgid(name: &str) -> Option<u32> {
    fs::read_to_string(marker_path(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn marker_path(name: &str) -> PathBuf {
    std::env::current_exe()
        .expect("current executable")
        .parent()
        .expect("executable directory")
        .join(name)
}

fn query_event(event: &str, detail: &str) {
    if let Ok(executable) = std::env::current_exe() {
        test_events::emit(&executable, event, detail);
    }
}

fn publish_version_pid() {
    let pid = std::process::id();
    let detail = match process_snapshot(pid) {
        Ok(snapshot) => format!(
            "pid={pid};pgid={};start={}.{}",
            snapshot.process_group_id, snapshot.start_seconds, snapshot.start_microseconds
        ),
        Err(_) => format!("pid={pid}"),
    };
    query_event("pid-published", &detail);
    let path = marker_path("query.pid");
    let temporary = marker_path(&format!("query.pid.tmp.{pid}"));
    let _ = fs::write(&temporary, format!("{detail}\n"));
    let _ = fs::rename(temporary, path);
}

fn write_version_output(output: &[u8]) {
    query_event("stdout-first-write", &format!("bytes={}", output.len()));
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let _ = stdout.write_all(output);
    let _ = stdout.flush();
    query_event("stdout-write-complete", &format!("bytes={}", output.len()));
}

fn version() {
    if marker_present(PRE_EXEC_GATE_MARKER) {
        query_event("pre-exec-entered", "waiting-for-release");
        while !marker_present(PRE_EXEC_RELEASE_MARKER) {
            thread::yield_now();
        }
    }
    query_event("exec-ready", "version");
    publish_version_pid();
    if marker_present(GROUP_ESCAPE_HOLD_MARKER)
        && let Some(target) = marker_pgid(JOIN_GROUP_OF_MARKER)
    {
        join_process_group(target).expect("join foreign process group");
        fs::write(marker_path(JOINED_GROUP_READY_MARKER), b"joined\n")
            .expect("write joined-group ready marker");
        query_event("group-escape", &format!("pgid={target}"));
        while marker_present(GROUP_ESCAPE_HOLD_MARKER)
            && !marker_present(GROUP_ESCAPE_RELEASE_MARKER)
        {
            thread::yield_now();
        }
    }
    if marker_present(SILENT_VERSION_DESCENDANT_MARKER) {
        #[allow(clippy::zombie_processes)]
        let grandchild = Command::new(std::env::current_exe().expect("current executable"))
            .arg("wait")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn silent grandchild");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open silent PID log");
        writeln!(log, "{}", std::process::id()).expect("record silent leader PID");
        writeln!(log, "{}", grandchild.id()).expect("record silent grandchild PID");
        query_event("descendant-spawned", &format!("pid={}", grandchild.id()));
        query_event("stdout-close", "leader-return");
        return;
    }
    if marker_present(VERSION_OUTPUT_DESCENDANT_MARKER) {
        write_version_output(b"test-fixture-1\n");
        #[allow(clippy::zombie_processes)]
        let grandchild = Command::new(std::env::current_exe().expect("current executable"))
            .arg("wait")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn output descendant");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open output PID log");
        writeln!(log, "{}", std::process::id()).expect("record output leader PID");
        writeln!(log, "{}", grandchild.id()).expect("record output grandchild PID");
        query_event("descendant-spawned", &format!("pid={}", grandchild.id()));
        query_event("stdout-close", "leader-return");
        return;
    }
    if marker_present(FAST_EXIT_VERSION_DESCENDANT_MARKER) {
        #[allow(clippy::zombie_processes)]
        let grandchild = Command::new(std::env::current_exe().expect("current executable"))
            .arg("wait")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fast-exit descendant");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open fast-exit PID log");
        writeln!(log, "{}", std::process::id()).expect("record fast-exit leader PID");
        writeln!(log, "{}", grandchild.id()).expect("record fast-exit descendant PID");
        query_event("descendant-spawned", &format!("pid={}", grandchild.id()));
        return;
    }
    if marker_present(HOLD_VERSION_STDOUT_MARKER) {
        // Models a version query whose direct child exits but a grandchild
        // inherits the stdout pipe and keeps it open. The query must still
        // be deadline-bounded and must kill the entire process group.
        #[allow(clippy::zombie_processes)]
        let grandchild = Command::new(std::env::current_exe().expect("current executable"))
            .arg("wait")
            .stdout(Stdio::inherit())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn grandchild");
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open hold PID log");
        writeln!(log, "{}", std::process::id()).expect("record direct child PID");
        writeln!(log, "{}", grandchild.id()).expect("record grandchild PID");
        query_event("descendant-spawned", &format!("pid={}", grandchild.id()));
        query_event("stdout-inherited", &format!("pid={}", grandchild.id()));
        return;
    }
    if marker_present(HANG_ON_VERSION_MARKER) {
        // Models an installed-version query whose subprocess never finishes:
        // record this instance's PID so the test can prove the bounded query
        // terminated and reaped exactly this child, then hang forever.
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open hang PID log");
        writeln!(log, "{}", std::process::id()).expect("record hang PID");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    if marker_present(CLOSE_VERSION_STDOUT_MARKER) {
        // Models a direct child that closes stdout while continuing to run:
        // the query must not block in an unbounded wait and must still kill
        // and reap the child at the deadline.
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open close-stdout PID log");
        writeln!(log, "{}", std::process::id()).expect("record close-stdout PID");
        write_version_output(b"test-fixture-1\n");
        close_fd(1).expect("close stdout");
        query_event("stdout-close", "explicit");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    if marker_present(FLOOD_VERSION_STDOUT_MARKER) {
        // Models output that crosses the query's byte bound without closing
        // the pipe: the query must stop at the exact bound, kill the group,
        // and reap the child instead of waiting for the deadline.
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(marker_path(HANG_PID_LOG))
            .expect("open flood PID log");
        writeln!(log, "{}", std::process::id()).expect("record flood PID");
        let payload = [b'x'; 1024];
        query_event("stdout-first-write", "bytes=1024");
        for _ in 0..48 {
            if std::io::stdout().write_all(&payload).is_err() {
                break;
            }
        }
        let _ = std::io::stdout().flush();
        query_event("stdout-write-complete", "bytes=49152");
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    if marker_present(INVALID_VERSION_OUTPUT_MARKER) {
        write_version_output(b"test-fixture-1\n\x01");
        query_event("stdout-close", "leader-return");
        return;
    }
    write_version_output(b"test-fixture-1\n");
    query_event("stdout-close", "leader-return");
}

fn serve() {
    // The fixture expresses SIGTERM-ignoring itself: when the marker is
    // present it ignores SIGTERM at the very start of `serve` and then
    // publishes the observable ready marker, so no supervisor API needs a
    // test-only behavior switch and the grace signal can never race the
    // disposition change.
    let ignore_sigterm = marker_present(IGNORE_SIGTERM_MARKER);
    if ignore_sigterm {
        ignore_signal(libc::SIGTERM).expect("ignore SIGTERM");
        fs::write(marker_path(IGNORE_SIGTERM_READY_MARKER), b"ready\n")
            .expect("write ignore-sigterm ready marker");
    }
    let mut hostname = None;
    let mut port = None;
    let mut arguments = std::env::args().skip(2);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--hostname" => hostname = arguments.next(),
            "--port" => port = arguments.next().and_then(|value| value.parse::<u16>().ok()),
            "--mdns" => {}
            _ => {
                std::process::exit(2);
            }
        }
    }
    let hostname = hostname.unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = port.unwrap_or(0);
    if let Some(target) = marker_pgid(JOIN_GROUP_OF_MARKER) {
        join_process_group(target).expect("join recorded process group");
        fs::write(marker_path(JOINED_GROUP_READY_MARKER), b"joined\n")
            .expect("write joined-group ready marker");
        // Keep the foreign group membership observable through the
        // production spawn identity-settle observation. The delay is a
        // fixture synchronization aid, not a product timing threshold.
        thread::sleep(Duration::from_millis(100));
    }
    if ignore_sigterm {
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    if marker_present(PORT_RESERVATION_HELD_MARKER) {
        query_event("bind-wait", "port-reservation");
        while !marker_present(PORT_RESERVATION_RELEASE_MARKER) {
            thread::yield_now();
        }
    }
    query_event("bind-requested", &format!("port={port}"));
    let listener =
        TcpListener::bind((hostname.as_str(), port)).expect("bind fixture health server");
    let _ = fs::write(marker_path(PORT_BIND_READY_MARKER), b"ready\n");
    query_event("bind-ready", &format!("port={port}"));
    if marker_present(HOLD_ENDPOINT_MARKER) {
        spawn_port_holder(&listener);
    }
    for connection in listener.incoming() {
        match connection {
            Ok(mut stream) => {
                if marker_present(JOIN_PARENT_GROUP_MARKER) {
                    let parent_group = parent_process_group().expect("parent process group");
                    join_process_group(parent_group).expect("join parent process group");
                }
                // The escape must complete before the health response is
                // written: the response therefore implies the group change,
                // and a supervisor that re-inspects identity after reading a
                // healthy response deterministically observes the escape.
                if let Some(target) = marker_pgid(ESCAPE_ON_ACCEPT_PGID_MARKER) {
                    join_process_group(target).expect("join foreign process group on accept");
                    fs::write(marker_path(JOINED_GROUP_READY_MARKER), b"joined\n")
                        .expect("write joined-group ready marker");
                }
                if marker_present(LEADER_EXIT_DESCENDANT_MARKER) {
                    #[allow(clippy::zombie_processes)]
                    let grandchild =
                        Command::new(std::env::current_exe().expect("current executable"))
                            .arg("ignore")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                            .expect("spawn leader-exit descendant");
                    let mut log = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(marker_path(LEADER_EXIT_DESCENDANT_PID_LOG))
                        .expect("open leader-exit PID log");
                    writeln!(log, "{}", std::process::id()).expect("record leader-exit leader PID");
                    writeln!(log, "{}", grandchild.id())
                        .expect("record leader-exit descendant PID");
                    query_event(
                        "leader-exit-descendant",
                        &format!("pid={}", grandchild.id()),
                    );
                    while !marker_present(LEADER_EXIT_DESCENDANT_READY) {
                        thread::yield_now();
                    }
                    respond_to_health(&mut stream);
                    // The health client reads until EOF. Close this one
                    // response connection before holding the leader alive
                    // behind the one-shot release marker, so the supervisor
                    // can reach Healthy and complete its identity recheck.
                    drop(stream);
                    while marker_present(LEADER_EXIT_DESCENDANT_MARKER) {
                        thread::yield_now();
                    }
                    return;
                }
                respond_to_health(&mut stream);
            }
            Err(_) => std::process::exit(1),
        }
    }
}

/// Test-only modeling of a real server's endpoint-release delay: a same-group
/// descendant keeps a duplicate of the listen socket bound for a moment after
/// the fixture leader dies, so a supervisor that respawns immediately after
/// observing the leader's exit still finds the endpoint occupied.
#[allow(clippy::zombie_processes)]
fn spawn_port_holder(listener: &TcpListener) {
    let duplicate = listener.try_clone().expect("duplicate listener for holder");
    Command::new(std::env::current_exe().expect("current executable"))
        .arg("hold")
        .stdin(Stdio::from(std::os::fd::OwnedFd::from(duplicate)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn port holder");
}

/// Keeps the inherited listen-socket duplicate (stdin) open until the
/// fixture leader is gone, then releases it after a short delay. SIGTERM is
/// ignored so the supervisor's process-group termination does not release
/// the socket early; the absolute deadline bounds any leak.
fn hold() {
    ignore_signal(libc::SIGTERM).expect("ignore SIGTERM");
    let parent = parent_process_id();
    // The production supervisor keeps an exited direct Child waitable while
    // it converges the authorized group. A test-only holder must therefore
    // model a short endpoint-release delay without waiting for the leader to
    // be reaped (a real child that requires reaping would otherwise create a
    // circular test fixture dependency).
    let release_after = std::time::Instant::now() + Duration::from_secs(1);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if !process_exists(parent) || std::time::Instant::now() >= release_after {
            thread::sleep(Duration::from_millis(1500));
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn respond_to_health(stream: &mut TcpStream) {
    let mut request = [0_u8; 8192];
    let Ok(length) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..length]);
    let password = std::env::var("OPENCODE_SERVER_PASSWORD").unwrap_or_default();
    let username =
        std::env::var("OPENCODE_SERVER_USERNAME").unwrap_or_else(|_| "opencode".to_owned());
    if !password.is_empty() {
        let expected = format!(
            "Authorization: Basic {}",
            STANDARD.encode(format!("{username}:{password}"))
        );
        if !request.lines().any(|line| line == expected) {
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
    }
    let body: &[u8] = if marker_present(UNHEALTHY_HEALTH_MARKER) {
        query_event("health-served", "healthy=false");
        br#"{"healthy":false,"version":"test-fixture-1"}"#
    } else {
        query_event("health-served", "healthy=true");
        br#"{"healthy":true,"version":"test-fixture-1"}"#
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
}
