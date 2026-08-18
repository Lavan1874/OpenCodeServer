#![cfg(feature = "test-fixture")]

use opencodeserver::config::{ConfigFile, write_config_atomically};
use opencodeserver::paths::AppPaths;
use opencodeserver::platform::{process_exists, process_snapshot, send_process_group_signal};
use opencodeserver::protocol::DesiredState;
use opencodeserver::runtime_state::RuntimeState;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static TEST_NONCE: AtomicU64 = AtomicU64::new(1);

fn test_paths() -> AppPaths {
    let nonce = TEST_NONCE.fetch_add(1, Ordering::Relaxed);
    AppPaths::from_support_dir(
        format!("/private/tmp/ocs-rtd-{}-{nonce}", std::process::id()).into(),
    )
}

fn write_test_config(paths: &AppPaths, port: u16) {
    write_config_atomically(
        &paths.config_file,
        &ConfigFile {
            port,
            username: format!("runtime-state-agent-{}", std::process::id()),
            executable_path: env!("CARGO_BIN_EXE_opencodeserver-test-child").to_owned(),
            ..ConfigFile::default()
        },
    )
    .expect("write test config");
}

fn available_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("reserve test port")
        .local_addr()
        .expect("port address")
        .port()
}

fn send_agent_command(paths: &AppPaths, command: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(&paths.control_socket) {
            Ok(mut stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("set agent read timeout");
                let request = format!("{{\"version\":6,\"command\":\"{command}\"}}\n");
                stream
                    .write_all(request.as_bytes())
                    .expect("write agent command");
                let mut line = String::new();
                let read = BufReader::new(stream)
                    .read_line(&mut line)
                    .expect("read agent response");
                assert!(read > 0, "OpenCodeServerAgent closed the control socket");
                return serde_json::from_str(&line).expect("decode agent response");
            }
            Err(error) if Instant::now() < deadline => {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ),
                    "unexpected control socket error: {error}"
                );
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("OpenCodeServerAgent did not become reachable: {error}"),
        }
    }
}

#[test]
fn crashed_opencodeserveragent_with_pending_launch_does_not_spawn_a_second_opencode() {
    let paths = test_paths();
    paths
        .ensure_directories()
        .expect("create support directory");
    write_test_config(&paths, available_port());
    RuntimeState {
        desired_state: DesiredState::Stopped,
        ..RuntimeState::default()
    }
    .save(&paths)
    .expect("save stopped state");

    // In the test-fixture build, save number four is the post-spawn process
    // record write: constructor, Running intent, launch marker, then record.
    // Failing that write in a separately spawned OpenCodeServerAgent leaves
    // the marker as the only durable evidence before the process is killed.
    let agent = env!("CARGO_BIN_EXE_OpenCodeServerAgent");
    let mut first_agent = Command::new(agent)
        .env("OPENCODESERVER_SUPPORT_DIR", &paths.support_dir)
        .env("OPENCODESERVER_TEST_RUNTIME_STATE_FAIL_AFTER", "3")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start first OpenCodeServerAgent");
    let start_response = send_agent_command(&paths, "start");
    assert_eq!(start_response["ok"], false);
    let old_pid = start_response["status"]["pid"]
        .as_u64()
        .map(|pid| pid as u32)
        .expect("failed start still reports the supervised OpenCode PID");
    let durable_state = RuntimeState::load(&paths).expect("load pending launch state");
    assert_eq!(durable_state.process, None);
    assert!(durable_state.launch_pending.is_some());
    assert!(
        process_exists(old_pid),
        "the old OpenCode must remain alive"
    );

    first_agent
        .kill()
        .expect("simulate OpenCodeServerAgent crash");
    first_agent.wait().expect("reap first OpenCodeServerAgent");
    assert!(
        process_snapshot(old_pid).is_ok(),
        "crashing OpenCodeServerAgent must not kill the supervised OpenCode"
    );

    let mut replacement_agent = Command::new(agent)
        .env("OPENCODESERVER_SUPPORT_DIR", &paths.support_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start replacement OpenCodeServerAgent");
    let replacement_status = send_agent_command(&paths, "status");
    assert_eq!(replacement_status["ok"], true);
    assert_eq!(replacement_status["status"]["server_state"], "failed");
    assert_eq!(replacement_status["status"]["pid"], serde_json::Value::Null);
    assert!(
        replacement_status["status"]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("not durably finalized")),
        "replacement OpenCodeServerAgent must report the pending launch"
    );
    assert!(
        process_exists(old_pid),
        "replacement OpenCodeServerAgent must not start a second OpenCode"
    );
    let stop_response = send_agent_command(&paths, "stop");
    assert_eq!(stop_response["ok"], false);
    assert_eq!(stop_response["status"]["desired_state"], "stopped");
    assert_eq!(stop_response["status"]["pid"], serde_json::Value::Null);
    assert_eq!(
        RuntimeState::load(&paths)
            .expect("load durable Stop intent")
            .desired_state,
        DesiredState::Stopped,
        "Stop intent must be durable even while the pending launch blocks signaling"
    );

    replacement_agent
        .kill()
        .expect("stop replacement OpenCodeServerAgent");
    replacement_agent
        .wait()
        .expect("reap replacement OpenCodeServerAgent");
    if let Ok(snapshot) = process_snapshot(old_pid) {
        send_process_group_signal(snapshot.process_group_id, libc::SIGTERM)
            .expect("stop the surviving fixture OpenCode");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_snapshot(old_pid).is_ok() {
        assert!(
            Instant::now() < deadline,
            "surviving fixture OpenCode did not stop"
        );
        thread::sleep(Duration::from_millis(20));
    }
    fs::remove_dir_all(&paths.support_dir).expect("remove test support directory");
}
