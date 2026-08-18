use opencodeserver::ipc::{
    IpcServer, PendingRequest, ResponseDisposition, Subscribers, encode_status_push,
};
use opencodeserver::paths::AppPaths;
use opencodeserver::platform::{
    ControlSignal, Event, EventQueue, LogLevel, block_control_signals, log, wait_for_control_signal,
};
use opencodeserver::protocol::{Response, StatusFingerprint};
use opencodeserver::supervisor::Supervisor;
use std::fs::File;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How often subscribers receive a status snapshot even when nothing
/// changed. It lets OpenCodeServer distinguish a quiet agent from a dead
/// connection.
const SUBSCRIBER_HEARTBEAT: Duration = Duration::from_secs(10);
/// Upper bound on a single kqueue wait so a missed event can never stall
/// supervision indefinitely.
const MAX_WAIT: Duration = Duration::from_secs(30);

fn main() {
    if let Err(error) = run() {
        log(
            LogLevel::Fault,
            &format!("OpenCodeServerAgent failed: {error}"),
        );
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    block_control_signals()?;
    let mut events = EventQueue::new()?;
    let waker = events.waker();
    let (signal_sender, signal_receiver) = mpsc::channel();
    thread::Builder::new()
        .name("signal-wait".to_owned())
        .spawn(move || {
            loop {
                match wait_for_control_signal() {
                    Ok(signal) => {
                        let should_exit =
                            matches!(signal, ControlSignal::Terminate | ControlSignal::Interrupt);
                        let sent = signal_sender.send(signal).is_ok();
                        waker.trigger_signal();
                        if !sent || should_exit {
                            break;
                        }
                    }
                    Err(error) => {
                        log(LogLevel::Fault, &format!("sigwait failed: {error}"));
                        break;
                    }
                }
            }
        })?;

    let paths = AppPaths::discover()?;
    // The IPC socket is bound before any heavy initialization so
    // OpenCodeServer can connect and observe startup progress immediately
    // after exec (ADR 0009).
    let mut server = IpcServer::bind(&paths)?;
    let mut supervisor = Supervisor::new(paths.clone())?;

    #[cfg(all(feature = "diagnostic-local-network", target_os = "macos"))]
    opencodeserver::diagnostic_local_network::start_once();

    events.watch_listener(server.listener_fd())?;
    let mut config_watch = watch_config(&events, &paths);
    let mut subscribers = Subscribers::new();
    let mut watched_pid: Option<u32> = None;
    let mut last_fingerprint = StatusFingerprint::from(&supervisor.status());
    let mut last_heartbeat = Instant::now();

    loop {
        // Keep the NOTE_EXIT registration in sync with the supervised PID.
        let pid = supervisor.process_pid();
        if pid != watched_pid {
            if let Some(previous) = watched_pid {
                let _ = events.unwatch_child(previous);
            }
            if let Some(current) = pid {
                // A reattached (non-child) OpenCode can exit and be reaped by
                // launchd between the last poll and this registration; the
                // EVFILT_PROC EV_ADD then fails (measured as ESRCH on macOS
                // 26). That race is routine, not fatal: log it and poll the
                // process immediately so the exit is classified through the
                // normal path (the per-tick poll remains as the standing
                // fallback) instead of crashing the agent into a KeepAlive
                // restart cycle.
                if let Err(error) = events.watch_child(current) {
                    log(
                        LogLevel::Error,
                        &format!("Unable to watch OpenCode PID {current} for exit: {error}"),
                    );
                    supervisor.poll_process_now();
                }
            }
            watched_pid = pid;
        }

        let mut should_exit = false;
        while let Ok(signal) = signal_receiver.try_recv() {
            match signal {
                // SIGHUP reloads the configuration, the same effect as the
                // config.plist vnode watch and the slow periodic recheck.
                ControlSignal::Hangup => supervisor.refresh_config_now(),
                ControlSignal::Terminate | ControlSignal::Interrupt => should_exit = true,
            }
        }
        if should_exit {
            supervisor.finish_version_query_for_shutdown();
            break;
        }

        supervisor.tick();

        let status = supervisor.status();
        let fingerprint = StatusFingerprint::from(&status);
        let changed = fingerprint != last_fingerprint;
        if changed {
            last_fingerprint = fingerprint;
        }
        if changed || last_heartbeat.elapsed() >= SUBSCRIBER_HEARTBEAT {
            if let Ok(line) = encode_status_push(&status) {
                subscribers.broadcast(&line);
            }
            last_heartbeat = Instant::now();
        }

        // The configuration file is replaced atomically on save, so the
        // vnode watch must be re-armed on the new file; if it is temporarily
        // missing, retry on the next iteration.
        if config_watch.is_none() {
            config_watch = watch_config(&events, &paths);
        }

        server.prepare_wait(&events)?;
        let now = Instant::now();
        let timeout = wait_timeout(&supervisor, &server, last_heartbeat, now);
        let mut listener_ready = false;
        for event in events.wait(timeout)? {
            match event {
                // Accept only after this immutable batch has been fully
                // dispatched. A descriptor closed by an earlier event cannot
                // then be reused by a new connection while a stale event for
                // that descriptor remains in this vector.
                Event::Listener => listener_ready = true,
                Event::PendingReadable(fd) => match server.handle_pending_readable(fd, &events) {
                    Ok(Some(request)) => dispatch_pending_request(
                        request,
                        &mut server,
                        &mut supervisor,
                        &events,
                        &mut subscribers,
                    ),
                    Ok(None) => {}
                    Err(error) => {
                        log(LogLevel::Error, &format!("IPC read failed: {error}"));
                    }
                },
                Event::StreamWritable(fd) => {
                    if let Err(error) =
                        server.handle_pending_writable(fd, &events, &mut subscribers)
                    {
                        log(LogLevel::Error, &format!("IPC write failed: {error}"));
                    }
                }
                Event::Stream(fd) => subscribers.handle_readable(fd),
                Event::ChildExit(_) => supervisor.poll_process_now(),
                Event::ConfigChanged => {
                    supervisor.refresh_config_now();
                    config_watch = watch_config(&events, &paths);
                }
                // Signals are drained at the top of the loop.
                Event::SignalWake => {}
            }
        }
        server.finish_event_batch(&events, listener_ready)?;
    }
    Ok(())
}

fn dispatch_pending_request(
    request: PendingRequest,
    server: &mut IpcServer,
    supervisor: &mut Supervisor,
    events: &EventQueue,
    subscribers: &mut Subscribers,
) {
    let result = match request {
        PendingRequest::OneShot { fd, command } => server.queue_response(
            fd,
            &supervisor.handle(command),
            ResponseDisposition::Close,
            events,
            subscribers,
        ),
        PendingRequest::Subscribe { fd } => server.queue_response(
            fd,
            &Response::success(supervisor.status()),
            ResponseDisposition::Subscribe,
            events,
            subscribers,
        ),
        PendingRequest::Reject { fd, message } => server.queue_response(
            fd,
            &Response::error(message, Some(supervisor.status())),
            ResponseDisposition::Close,
            events,
            subscribers,
        ),
    };
    if let Err(error) = result {
        log(
            LogLevel::Error,
            &format!("IPC response could not be queued: {error}"),
        );
    }
}

fn watch_config(events: &EventQueue, paths: &AppPaths) -> Option<File> {
    let file = File::open(&paths.config_file).ok()?;
    match events.watch_config(&file) {
        Ok(()) => Some(file),
        Err(error) => {
            log(
                LogLevel::Error,
                &format!("Unable to watch config.plist for changes: {error}"),
            );
            None
        }
    }
}

fn wait_timeout(
    supervisor: &Supervisor,
    server: &IpcServer,
    last_heartbeat: Instant,
    now: Instant,
) -> Duration {
    let mut timeout = (last_heartbeat + SUBSCRIBER_HEARTBEAT).saturating_duration_since(now);
    if let Some(deadline) = supervisor.next_deadline(now) {
        timeout = timeout.min(deadline.saturating_duration_since(now));
    }
    if let Some(deadline) = server.next_deadline() {
        timeout = timeout.min(deadline.saturating_duration_since(now));
    }
    timeout.min(MAX_WAIT)
}
