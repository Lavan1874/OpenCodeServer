use crate::paths::AppPaths;
use crate::platform::{
    EventQueue, LogLevel, effective_uid, log, peer_effective_uid, set_no_sigpipe,
};
use crate::protocol::{Command, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Request, Response, Status};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PENDING_HANDSHAKES: usize = 16;
const MAX_ACCEPTS_PER_BATCH: usize = MAX_PENDING_HANDSHAKES;
/// OpenCodeServer is the only legitimate subscriber, but a reconnect can
/// briefly overlap with the previous connection. The small cap bounds the
/// kqueue watches and descriptors a same-user peer can accumulate through
/// completed subscribe handshakes; beyond it the newest subscription is
/// refused and closed, consistent with the MAX_PENDING_HANDSHAKES bound.
const MAX_SUBSCRIBERS: usize = 4;
const READ_BUDGET: usize = 4 * 1024;
const WRITE_BUDGET: usize = 16 * 1024;

pub struct IpcServer {
    listener: UnixListener,
    socket_path: std::path::PathBuf,
    pending: HashMap<RawFd, PendingConnection>,
    listener_enabled: bool,
}

struct PendingConnection {
    stream: UnixStream,
    deadline: Instant,
    state: PendingState,
}

enum PendingState {
    Reading(Vec<u8>),
    AwaitingResponse,
    Writing {
        response: Vec<u8>,
        offset: usize,
        disposition: ResponseDisposition,
    },
}

#[derive(Clone, Copy)]
pub enum ResponseDisposition {
    Close,
    Subscribe,
}

pub enum PendingRequest {
    OneShot { fd: RawFd, command: Command },
    Subscribe { fd: RawFd },
    Reject { fd: RawFd, message: &'static str },
}

impl IpcServer {
    pub fn bind(paths: &AppPaths) -> io::Result<Self> {
        paths.ensure_directories()?;
        prepare_socket_path(&paths.control_socket)?;
        let listener = UnixListener::bind(&paths.control_socket)?;
        fs::set_permissions(&paths.control_socket, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            socket_path: paths.control_socket.clone(),
            pending: HashMap::new(),
            listener_enabled: true,
        })
    }

    pub fn listener_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    /// Performs deadline cleanup before sleeping. Listener re-enablement is a
    /// loop-boundary operation: no new descriptor can reuse an fd while the
    /// caller is still dispatching a previously returned kqueue batch.
    pub fn prepare_wait(&mut self, events: &EventQueue) -> io::Result<()> {
        self.maintain(events, false, Instant::now())
    }

    /// Finishes one immutable kqueue batch. A listener event is only a note to
    /// accept here, after every fd event in that batch has been consumed. This
    /// prevents a newly accepted descriptor from receiving a stale event for a
    /// connection closed earlier in the same batch.
    pub fn finish_event_batch(
        &mut self,
        events: &EventQueue,
        listener_ready: bool,
    ) -> io::Result<()> {
        self.maintain(events, listener_ready, Instant::now())
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|connection| connection.deadline)
            .min()
    }

    pub fn handle_pending_readable(
        &mut self,
        fd: RawFd,
        events: &EventQueue,
    ) -> io::Result<Option<PendingRequest>> {
        let Some(deadline) = self.pending.get(&fd).map(|connection| connection.deadline) else {
            // `close` automatically removes fd filters from kqueue, but an
            // event already copied into the current batch can still arrive.
            return Ok(None);
        };
        if Instant::now() >= deadline {
            self.pending.remove(&fd);
            return Ok(None);
        }

        let mut bytes = [0_u8; READ_BUDGET];
        let read_result = {
            let connection = self.pending.get_mut(&fd).expect("checked pending fd");
            let PendingState::Reading(buffer) = &mut connection.state else {
                return Ok(None);
            };
            let remaining = MAX_MESSAGE_BYTES as usize - buffer.len();
            if remaining == 0 {
                Ok(0)
            } else {
                connection
                    .stream
                    .read(&mut bytes[..remaining.min(READ_BUDGET)])
            }
        };

        let request_action = match read_result {
            Ok(0) => {
                let full_without_newline = self.pending.get(&fd).is_some_and(|connection| {
                    matches!(
                        &connection.state,
                        PendingState::Reading(buffer)
                            if buffer.len() == MAX_MESSAGE_BYTES as usize
                    )
                });
                if full_without_newline {
                    Some(RequestAction::Reject("request exceeds 64 KiB"))
                } else {
                    self.pending.remove(&fd);
                    None
                }
            }
            Ok(length) => {
                let connection = self.pending.get_mut(&fd).expect("checked pending fd");
                let PendingState::Reading(buffer) = &mut connection.state else {
                    return Ok(None);
                };
                buffer.extend_from_slice(&bytes[..length]);
                if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
                    Some(classify_request(&buffer[..=newline]))
                } else if buffer.len() == MAX_MESSAGE_BYTES as usize {
                    // The complete wire message includes its LF. Once all
                    // 65,536 bytes are occupied without one, no legal message
                    // can be completed; reject without another read syscall.
                    Some(RequestAction::Reject("request exceeds 64 KiB"))
                } else {
                    None
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
            Err(error) => {
                self.pending.remove(&fd);
                return Err(error);
            }
        };

        if Instant::now() >= deadline {
            self.pending.remove(&fd);
            return Ok(None);
        }
        let Some(request_action) = request_action else {
            return Ok(None);
        };

        if let Err(error) = events.unwatch_stream_readable(fd) {
            self.pending.remove(&fd);
            return Err(error);
        }
        if let Some(connection) = self.pending.get_mut(&fd) {
            connection.state = PendingState::AwaitingResponse;
        }
        Ok(Some(match request_action {
            RequestAction::OneShot(command) => PendingRequest::OneShot { fd, command },
            RequestAction::Subscribe => PendingRequest::Subscribe { fd },
            RequestAction::Reject(message) => PendingRequest::Reject { fd, message },
        }))
    }

    pub fn queue_response(
        &mut self,
        fd: RawFd,
        response: &Response,
        disposition: ResponseDisposition,
        events: &EventQueue,
        subscribers: &mut Subscribers,
    ) -> io::Result<()> {
        let encoded = match encode_response(response) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.pending.remove(&fd);
                return Err(error);
            }
        };
        let Some(connection) = self.pending.get_mut(&fd) else {
            return Ok(());
        };
        if Instant::now() >= connection.deadline {
            self.pending.remove(&fd);
            return Ok(());
        }
        if !matches!(connection.state, PendingState::AwaitingResponse) {
            self.pending.remove(&fd);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IPC response queued for a connection in the wrong state",
            ));
        }
        connection.state = PendingState::Writing {
            response: encoded,
            offset: 0,
            disposition,
        };
        self.advance_write(fd, events, subscribers, false)
    }

    pub fn handle_pending_writable(
        &mut self,
        fd: RawFd,
        events: &EventQueue,
        subscribers: &mut Subscribers,
    ) -> io::Result<()> {
        self.advance_write(fd, events, subscribers, true)
    }

    /// Attempts one bounded nonblocking write. The first attempt happens
    /// immediately after entering `Writing`; EVFILT_WRITE is armed only when
    /// that attempt is partial or returns EAGAIN. Darwin can withhold the
    /// initial writable event for a Unix socket whose peer has a receive
    /// window below the sender low-water mark even though a direct write can
    /// still fill the local send buffer.
    fn advance_write(
        &mut self,
        fd: RawFd,
        events: &EventQueue,
        subscribers: &mut Subscribers,
        writable_armed: bool,
    ) -> io::Result<()> {
        let Some(deadline) = self.pending.get(&fd).map(|connection| connection.deadline) else {
            return Ok(());
        };
        if Instant::now() >= deadline {
            self.pending.remove(&fd);
            return Ok(());
        }

        enum WriteResult {
            Keep,
            Close,
            Subscribe,
        }
        let result = {
            let connection = self.pending.get_mut(&fd).expect("checked pending fd");
            let PendingState::Writing {
                response,
                offset,
                disposition,
            } = &mut connection.state
            else {
                return Ok(());
            };
            let end = (*offset + WRITE_BUDGET).min(response.len());
            match connection.stream.write(&response[*offset..end]) {
                Ok(0) => WriteResult::Close,
                Ok(written) => {
                    *offset += written;
                    if *offset == response.len() {
                        match disposition {
                            ResponseDisposition::Close => WriteResult::Close,
                            ResponseDisposition::Subscribe => WriteResult::Subscribe,
                        }
                    } else {
                        WriteResult::Keep
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => WriteResult::Keep,
                Err(error) => {
                    self.pending.remove(&fd);
                    return Err(error);
                }
            }
        };

        if Instant::now() >= deadline {
            self.pending.remove(&fd);
            return Ok(());
        }
        match result {
            WriteResult::Keep => {
                if !writable_armed && let Err(error) = events.watch_stream_writable(fd) {
                    self.pending.remove(&fd);
                    return Err(error);
                }
                Ok(())
            }
            WriteResult::Close => {
                // Darwin removes every fd-backed kevent on the last close.
                // Explicit EV_DELETE here would be redundant and cannot erase
                // events already copied into the current batch.
                self.pending.remove(&fd);
                Ok(())
            }
            WriteResult::Subscribe => {
                if writable_armed && let Err(error) = events.unwatch_stream_writable(fd) {
                    self.pending.remove(&fd);
                    return Err(error);
                }
                if let Err(error) = events.watch_stream(fd) {
                    self.pending.remove(&fd);
                    return Err(error);
                }
                if let Some(connection) = self.pending.remove(&fd) {
                    subscribers.add(connection.stream);
                }
                Ok(())
            }
        }
    }

    fn maintain(
        &mut self,
        events: &EventQueue,
        listener_ready: bool,
        now: Instant,
    ) -> io::Result<()> {
        self.pending
            .retain(|_, connection| connection.deadline > now);

        // Only accept a listener event that was returned while the listener
        // was already enabled. Re-enabling a backpressured listener waits for
        // the next kqueue batch before accepting its backlog.
        if listener_ready && self.listener_enabled && self.pending.len() < MAX_PENDING_HANDSHAKES {
            self.accept_connections(events);
        }
        if self.pending.len() >= MAX_PENDING_HANDSHAKES && self.listener_enabled {
            events.disable_listener()?;
            self.listener_enabled = false;
        } else if self.pending.len() < MAX_PENDING_HANDSHAKES && !self.listener_enabled {
            events.enable_listener()?;
            self.listener_enabled = true;
        }
        Ok(())
    }

    fn accept_connections(&mut self, events: &EventQueue) {
        let mut attempts = 0;
        while self.pending.len() < MAX_PENDING_HANDSHAKES && attempts < MAX_ACCEPTS_PER_BATCH {
            let (stream, _) = match self.listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    log(LogLevel::Error, &format!("IPC accept failed: {error}"));
                    break;
                }
            };
            attempts += 1;
            let accepted_at = Instant::now();
            if let Err(error) = self.register_accepted(stream, accepted_at, events) {
                log(
                    LogLevel::Error,
                    &format!("IPC connection rejected: {error}"),
                );
            }
        }
    }

    fn register_accepted(
        &mut self,
        stream: UnixStream,
        accepted_at: Instant,
        events: &EventQueue,
    ) -> io::Result<()> {
        // Scoped to this socket per ADR 0007 and applied before authentication
        // or any I/O. The process-wide SIGPIPE disposition is unchanged.
        set_no_sigpipe(&stream)?;
        let peer_uid = peer_effective_uid(&stream)?;
        if peer_uid != effective_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC peer does not belong to the current user",
            ));
        }
        stream.set_nonblocking(true)?;
        let deadline = accepted_at + HANDSHAKE_TIMEOUT;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "IPC handshake expired during authentication",
            ));
        }
        let fd = stream.as_raw_fd();
        events.watch_pending_readable(fd)?;
        self.pending.insert(
            fd,
            PendingConnection {
                stream,
                deadline,
                state: PendingState::Reading(Vec::new()),
            },
        );
        Ok(())
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Long-lived IPC connections that receive a pushed status snapshot on every
/// status-fingerprint change and a periodic heartbeat (ADR 0010).
///
/// Writes are non-blocking single attempts: a client that cannot keep up is
/// dropped and reconnects through its own backoff logic, so a stalled peer
/// can never block the event loop.
#[derive(Default)]
pub struct Subscribers {
    streams: Vec<UnixStream>,
}

impl Subscribers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a completed subscription. Beyond MAX_SUBSCRIBERS the newest
    /// connection is refused: dropping the stream closes it, which also
    /// removes its kqueue registrations, and the client reconnects through
    /// its own bounded backoff (ADR 0010).
    pub fn add(&mut self, stream: UnixStream) {
        if self.streams.len() >= MAX_SUBSCRIBERS {
            log(
                LogLevel::Error,
                "IPC subscriber limit reached; refusing the newest subscription",
            );
            return;
        }
        self.streams.push(stream);
    }

    pub fn broadcast(&mut self, line: &[u8]) {
        self.streams.retain_mut(|stream| match stream.write(line) {
            Ok(written) => written == line.len(),
            Err(_) => false,
        });
    }

    /// Handles a readable subscriber socket. Subscribers never send data, so
    /// an orderly close (EOF) and unexpected bytes both end the subscription.
    pub fn handle_readable(&mut self, fd: RawFd) {
        let mut buffer = [0_u8; 256];
        self.streams.retain_mut(|stream| {
            if stream.as_raw_fd() != fd {
                return true;
            }
            matches!(
                stream.read(&mut buffer),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock
            )
        });
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.streams.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }
}

pub fn send_request(paths: &AppPaths, request: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(&paths.control_socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    set_no_sigpipe(&stream)?;
    let mut encoded = serde_json::to_vec(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    encoded.push(b'\n');
    stream.write_all(&encoded)?;
    stream.flush()?;

    let mut line = Vec::new();
    BufReader::new(stream)
        .take(MAX_MESSAGE_BYTES + 1)
        .read_until(b'\n', &mut line)?;
    if line.len() as u64 > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenCodeServerAgent response exceeds 64 KiB",
        ));
    }
    let response: Response = serde_json::from_slice(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if response.version != PROTOCOL_VERSION
        || response
            .status
            .as_ref()
            .is_some_and(|status| status.protocol_version != PROTOCOL_VERSION)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenCodeServerAgent response uses an unsupported IPC protocol version",
        ));
    }
    Ok(response)
}

/// Encodes a pushed status snapshot for subscribers in the same response
/// envelope one-shot clients already decode.
pub fn encode_status_push(status: &Status) -> io::Result<Vec<u8>> {
    encode_response(&Response::success(status.clone()))
}

enum RequestAction {
    OneShot(Command),
    Subscribe,
    Reject(&'static str),
}

fn classify_request(line: &[u8]) -> RequestAction {
    if line.len() as u64 > MAX_MESSAGE_BYTES {
        return RequestAction::Reject("request exceeds 64 KiB");
    }
    match serde_json::from_slice::<Request>(line) {
        Ok(request) => {
            if request.version != PROTOCOL_VERSION {
                RequestAction::Reject("unsupported IPC protocol version")
            } else if request.command == Command::Subscribe {
                RequestAction::Subscribe
            } else {
                RequestAction::OneShot(request.command)
            }
        }
        Err(_) => RequestAction::Reject("request JSON is invalid"),
    }
}

fn encode_response(response: &Response) -> io::Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    // Wire semantics (ADR 0010): a complete message is the JSON body plus its
    // terminating newline, and the whole line must fit MAX_MESSAGE_BYTES.
    // Counting the newline here keeps the write side aligned with the read
    // side, which measures the line including the terminator.
    if encoded.len() as u64 + 1 > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OpenCodeServerAgent response exceeds 64 KiB",
        ));
    }
    encoded.push(b'\n');
    Ok(encoded)
}

fn prepare_socket_path(path: &Path) -> io::Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "control socket path exists and is not a Unix-domain socket",
        ));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "another OpenCodeServerAgent is already listening",
        ));
    }
    fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        ActionCapabilities, DesiredState, FdaState, HealthState, PasswordState, ServerState,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static TEST_NONCE: AtomicU64 = AtomicU64::new(1);

    fn test_server(label: &str) -> (std::path::PathBuf, IpcServer, EventQueue) {
        let nonce = TEST_NONCE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("ocs-ipc-{label}-{}-{nonce}", std::process::id()));
        let paths = AppPaths::from_support_dir(root.clone());
        let server = IpcServer::bind(&paths).expect("bind test IPC server");
        let mut events = EventQueue::new().expect("event queue");
        events
            .watch_listener(server.listener_fd())
            .expect("watch listener");
        (root, server, events)
    }

    fn accept_ready(server: &mut IpcServer, events: &EventQueue) {
        let batch = events
            .wait(Duration::from_secs(2))
            .expect("wait for listener");
        assert!(batch.contains(&crate::platform::Event::Listener));
        server
            .finish_event_batch(events, true)
            .expect("accept ready connections");
    }

    fn wait_for_request(server: &mut IpcServer, events: &EventQueue) -> PendingRequest {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            assert!(Instant::now() < deadline, "request was not framed");
            let batch = events
                .wait(Duration::from_millis(100))
                .expect("wait for request data");
            let mut listener_ready = false;
            let mut request = None;
            for event in batch {
                match event {
                    crate::platform::Event::Listener => listener_ready = true,
                    crate::platform::Event::PendingReadable(fd) => {
                        request = server
                            .handle_pending_readable(fd, events)
                            .expect("read pending request");
                    }
                    _ => {}
                }
            }
            server
                .finish_event_batch(events, listener_ready)
                .expect("finish request batch");
            if let Some(request) = request {
                return request;
            }
        }
    }

    fn drive_response(
        server: &mut IpcServer,
        events: &EventQueue,
        subscribers: &mut Subscribers,
        fd: RawFd,
    ) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.pending.contains_key(&fd) {
            assert!(Instant::now() < deadline, "response was not completed");
            let batch = events
                .wait(Duration::from_millis(100))
                .expect("wait for response write");
            let mut listener_ready = false;
            for event in batch {
                match event {
                    crate::platform::Event::Listener => listener_ready = true,
                    crate::platform::Event::StreamWritable(writable) if writable == fd => server
                        .handle_pending_writable(writable, events, subscribers)
                        .expect("write pending response"),
                    _ => {}
                }
            }
            server
                .finish_event_batch(events, listener_ready)
                .expect("finish response batch");
        }
    }

    fn test_status() -> Status {
        Status {
            protocol_version: PROTOCOL_VERSION,
            agent_version: "test".to_owned(),
            agent_uptime_seconds: 0,
            desired_state: DesiredState::Stopped,
            server_state: ServerState::Stopped,
            health: HealthState::Unknown,
            fda: FdaState::UnableToDetermine,
            uptime_seconds: None,
            endpoint: "127.0.0.1:4096".to_owned(),
            username: "test-user".to_owned(),
            password_state: PasswordState::NotConfigured,
            authentication_enabled: false,
            action_capabilities: ActionCapabilities {
                start: true,
                stop: false,
                restart: true,
                continue_stop: false,
                force_stop: false,
            },
            installed_version: None,
            running_version: None,
            version_pending: false,
            config_pending: false,
            config_error: None,
            last_error: None,
            pid: None,
            stop_grace_remaining_seconds: None,
            notification: None,
            process_started_at_unix_seconds: None,
            bundle_version: "test".to_owned(),
        }
    }

    fn padded_status_request(wire_length: usize, include_newline: bool) -> Vec<u8> {
        let mut request = br#"{"version":6,"command":"status"}"#.to_vec();
        let terminator = usize::from(include_newline);
        assert!(request.len() + terminator <= wire_length);
        request.resize(wire_length - terminator, b' ');
        if include_newline {
            request.push(b'\n');
        }
        request
    }

    fn frame_request_of_length(wire_length: usize, include_newline: bool) -> PendingRequest {
        let (root, mut server, events) = test_server("wire-boundary");
        let client = UnixStream::connect(&server.socket_path).expect("connect boundary client");
        accept_ready(&mut server, &events);
        let payload = padded_status_request(wire_length, include_newline);
        let mut writer = client.try_clone().expect("clone boundary client");
        let write = thread::spawn(move || writer.write_all(&payload));
        let request = wait_for_request(&mut server, &events);
        write
            .join()
            .expect("join boundary writer")
            .expect("write boundary request");
        drop(client);
        drop(server);
        fs::remove_dir_all(root).expect("remove boundary support directory");
        request
    }

    #[test]
    fn refuses_to_replace_a_regular_file() {
        let root = std::env::temp_dir().join(format!("ocs-socket-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&root);
        let path = root.join("control.sock");
        fs::write(&path, b"not a socket").expect("fixture");
        assert_eq!(
            prepare_socket_path(&path).expect_err("must reject").kind(),
            io::ErrorKind::AlreadyExists
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn classifies_protocol_versions() {
        let one_shot = classify_request(br#"{"version":6,"command":"status"}"#);
        assert!(matches!(one_shot, RequestAction::OneShot(Command::Status)));

        let subscribe = classify_request(br#"{"version":6,"command":"subscribe"}"#);
        assert!(matches!(subscribe, RequestAction::Subscribe));

        let stale = classify_request(br#"{"version":1,"command":"status"}"#);
        assert!(matches!(stale, RequestAction::Reject(message) if message.contains("unsupported")));

        let future = classify_request(br#"{"version":99,"command":"status"}"#);
        assert!(
            matches!(future, RequestAction::Reject(message) if message.contains("unsupported"))
        );

        let invalid = classify_request(b"not json");
        assert!(matches!(invalid, RequestAction::Reject(message) if message.contains("invalid")));
    }

    #[test]
    fn encode_response_counts_the_terminating_newline_toward_the_limit() {
        // Wire semantics: a complete message is body + newline and the whole
        // line must fit MAX_MESSAGE_BYTES, matching the read side.
        let overhead = serde_json::to_vec(&Response::error("", None))
            .expect("serialize")
            .len() as u64;
        // The longest body whose line (body + '\n') still fits must encode.
        let fits = "x".repeat((MAX_MESSAGE_BYTES - 1 - overhead) as usize);
        let encoded = encode_response(&Response::error(fits, None)).expect("boundary fits");
        assert_eq!(encoded.len() as u64, MAX_MESSAGE_BYTES);
        // One byte more must be rejected before anything reaches the wire.
        let too_long = "x".repeat((MAX_MESSAGE_BYTES - overhead) as usize);
        let rejected = encode_response(&Response::error(too_long, None))
            .expect_err("a line longer than 64 KiB must be rejected");
        assert_eq!(rejected.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn broadcast_reaches_live_subscribers() {
        let (agent_side, client) = UnixStream::pair().expect("socket pair");
        agent_side.set_nonblocking(true).expect("nonblocking");
        let mut subscribers = Subscribers::new();
        subscribers.add(agent_side);
        subscribers.broadcast(b"{\"ok\":true}\n");
        let mut received = String::new();
        let mut client = BufReader::new(client);
        client.read_line(&mut received).expect("read push");
        assert_eq!(received, "{\"ok\":true}\n");
        assert_eq!(subscribers.len(), 1);
    }

    #[test]
    fn subscriber_cap_refuses_and_closes_the_newest_connection() {
        let mut subscribers = Subscribers::new();
        let mut clients = Vec::new();
        for _ in 0..MAX_SUBSCRIBERS {
            let (agent_side, client) = UnixStream::pair().expect("socket pair");
            agent_side.set_nonblocking(true).expect("nonblocking");
            subscribers.add(agent_side);
            clients.push(client);
        }
        assert_eq!(subscribers.len(), MAX_SUBSCRIBERS);

        let (overflow_side, mut overflow_client) = UnixStream::pair().expect("socket pair");
        overflow_side.set_nonblocking(true).expect("nonblocking");
        subscribers.add(overflow_side);
        assert_eq!(
            subscribers.len(),
            MAX_SUBSCRIBERS,
            "a subscription beyond the cap is refused"
        );
        // The refused connection is closed, so its client observes EOF.
        let mut byte = [0_u8; 1];
        assert_eq!(overflow_client.read(&mut byte).expect("read EOF"), 0);

        // Established subscribers are unaffected and still receive pushes.
        subscribers.broadcast(b"{\"ok\":true}\n");
        let mut received = String::new();
        let mut client = BufReader::new(clients.first().expect("first client"));
        client.read_line(&mut received).expect("read push");
        assert_eq!(received, "{\"ok\":true}\n");
        assert_eq!(subscribers.len(), MAX_SUBSCRIBERS);
    }

    #[test]
    fn fast_request_completes_in_one_read() {
        let (root, mut server, events) = test_server("fast-request");
        let mut client = UnixStream::connect(&server.socket_path).expect("connect fast client");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set response timeout");
        accept_ready(&mut server, &events);
        client
            .write_all(b"{\"version\":6,\"command\":\"status\"}\n")
            .expect("write complete request");
        let PendingRequest::OneShot {
            fd,
            command: Command::Status,
        } = wait_for_request(&mut server, &events)
        else {
            panic!("fast request was not classified as status");
        };
        let mut subscribers = Subscribers::new();
        server
            .queue_response(
                fd,
                &Response::success(test_status()),
                ResponseDisposition::Close,
                &events,
                &mut subscribers,
            )
            .expect("queue fast response");
        drive_response(&mut server, &events, &mut subscribers, fd);
        let mut line = String::new();
        BufReader::new(client)
            .read_line(&mut line)
            .expect("read fast response");
        let response: Response = serde_json::from_str(&line).expect("decode fast response");
        assert_eq!(response.version, PROTOCOL_VERSION);
        assert!(response.ok);
        assert_eq!(
            response.status.expect("response status").server_state,
            ServerState::Stopped
        );
        drop(server);
        fs::remove_dir_all(root).expect("remove fast support directory");
    }

    #[test]
    fn fragmented_request_keeps_the_accept_time_deadline() {
        let (root, mut server, events) = test_server("fragmented-request");
        let mut client =
            UnixStream::connect(&server.socket_path).expect("connect fragmented client");
        accept_ready(&mut server, &events);
        let fd = *server.pending.keys().next().expect("accepted pending fd");
        let original_deadline = server.pending[&fd].deadline;

        client
            .write_all(b"{\"version\":6,")
            .expect("first fragment");
        let first = events
            .wait(Duration::from_secs(1))
            .expect("first fragment event");
        assert_eq!(first, vec![crate::platform::Event::PendingReadable(fd)]);
        assert!(
            server
                .handle_pending_readable(fd, &events)
                .expect("read first fragment")
                .is_none()
        );
        assert_eq!(server.pending[&fd].deadline, original_deadline);

        thread::sleep(Duration::from_millis(100));
        client
            .write_all(b"\"command\":\"status\"}")
            .expect("second fragment");
        let second = events
            .wait(Duration::from_secs(1))
            .expect("second fragment event");
        assert_eq!(second, vec![crate::platform::Event::PendingReadable(fd)]);
        assert!(
            server
                .handle_pending_readable(fd, &events)
                .expect("read second fragment")
                .is_none()
        );
        assert_eq!(server.pending[&fd].deadline, original_deadline);

        thread::sleep(Duration::from_millis(100));
        client.write_all(b"\n").expect("terminating newline");
        let request = wait_for_request(&mut server, &events);
        assert!(matches!(
            request,
            PendingRequest::OneShot {
                fd: request_fd,
                command: Command::Status
            } if request_fd == fd
        ));
        assert_eq!(server.pending[&fd].deadline, original_deadline);
        drop(client);
        drop(server);
        fs::remove_dir_all(root).expect("remove fragmented support directory");
    }

    #[test]
    fn request_wire_length_65535_is_accepted() {
        assert!(matches!(
            frame_request_of_length(65_535, true),
            PendingRequest::OneShot {
                command: Command::Status,
                ..
            }
        ));
    }

    #[test]
    fn request_wire_length_65536_is_accepted() {
        assert!(matches!(
            frame_request_of_length(65_536, true),
            PendingRequest::OneShot {
                command: Command::Status,
                ..
            }
        ));
    }

    #[test]
    fn request_wire_length_65537_is_rejected() {
        assert!(matches!(
            frame_request_of_length(65_537, true),
            PendingRequest::Reject { message, .. } if message.contains("exceeds")
        ));
    }

    #[test]
    fn full_65536_byte_buffer_without_lf_is_rejected_immediately() {
        assert!(matches!(
            frame_request_of_length(65_536, false),
            PendingRequest::Reject { message, .. } if message.contains("exceeds")
        ));
    }

    #[test]
    fn slow_reader_receives_response_across_multiple_writes() {
        let (root, mut server, events) = test_server("multi-write");
        let (agent_side, mut client) = UnixStream::pair().expect("response socket pair");
        agent_side
            .set_nonblocking(true)
            .expect("nonblocking server socket");
        set_no_sigpipe(&agent_side).expect("set SO_NOSIGPIPE");
        let actual_send_buffer = crate::platform::set_send_buffer_size_for_tests(&agent_side, 4096)
            .expect("limit server send buffer");
        assert!(actual_send_buffer < 32 * 1024);
        let actual_receive_buffer =
            crate::platform::set_receive_buffer_size_for_tests(&client, 4096)
                .expect("limit client receive buffer");
        assert!(actual_receive_buffer < 32 * 1024);
        let fd = agent_side.as_raw_fd();
        server.pending.insert(
            fd,
            PendingConnection {
                stream: agent_side,
                deadline: Instant::now() + HANDSHAKE_TIMEOUT,
                state: PendingState::AwaitingResponse,
            },
        );
        let response = Response::error("x".repeat(48 * 1024), None);
        let encoded = encode_response(&response).expect("encode large response");
        let mut subscribers = Subscribers::new();
        server
            .queue_response(
                fd,
                &response,
                ResponseDisposition::Close,
                &events,
                &mut subscribers,
            )
            .expect("queue large response");

        let PendingState::Writing { offset, .. } = &server.pending[&fd].state else {
            panic!("response completed in one write despite a full send buffer");
        };
        assert!(*offset > 0);
        assert!(*offset < encoded.len());

        let mut received = Vec::new();
        client
            .set_nonblocking(true)
            .expect("nonblocking response reader");
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.pending.contains_key(&fd) {
            assert!(Instant::now() < deadline, "multi-write response stalled");
            let mut chunk = [0_u8; 2048];
            loop {
                match client.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(length) => received.extend_from_slice(&chunk[..length]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("read partial response: {error}"),
                }
            }
            let batch = events
                .wait(Duration::from_millis(100))
                .expect("wait for subsequent response write");
            for event in batch {
                if event == crate::platform::Event::StreamWritable(fd) {
                    server
                        .handle_pending_writable(fd, &events, &mut subscribers)
                        .expect("continue response write");
                }
            }
        }
        client.set_nonblocking(false).expect("blocking final drain");
        client
            .read_to_end(&mut received)
            .expect("drain closed response");
        assert_eq!(received, encoded);
        drop(server);
        fs::remove_dir_all(root).expect("remove multi-write support directory");
    }

    #[test]
    fn subscription_promotes_after_its_initial_response_is_written() {
        let (root, mut server, events) = test_server("subscribe-promotion");
        let mut client = UnixStream::connect(&server.socket_path).expect("connect subscriber");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("subscription read timeout");
        accept_ready(&mut server, &events);
        client
            .write_all(b"{\"version\":6,\"command\":\"subscribe\"}\n")
            .expect("write subscribe request");
        let PendingRequest::Subscribe { fd } = wait_for_request(&mut server, &events) else {
            panic!("subscribe request was not classified");
        };
        let actual_send_buffer =
            crate::platform::set_send_buffer_size_for_tests(&server.pending[&fd].stream, 4096)
                .expect("limit subscription send buffer");
        assert!(actual_send_buffer < 32 * 1024);
        let actual_receive_buffer =
            crate::platform::set_receive_buffer_size_for_tests(&client, 4096)
                .expect("limit subscription receive buffer");
        assert!(actual_receive_buffer < 32 * 1024);
        let mut status = test_status();
        status.last_error = Some("x".repeat(48 * 1024));
        let response = Response::success(status);
        let encoded = encode_response(&response).expect("encode subscription response");
        let mut subscribers = Subscribers::new();
        server
            .queue_response(
                fd,
                &response,
                ResponseDisposition::Subscribe,
                &events,
                &mut subscribers,
            )
            .expect("queue initial subscription response");
        assert!(server.pending.contains_key(&fd));
        assert!(subscribers.is_empty());
        let PendingState::Writing { offset, .. } = &server.pending[&fd].state else {
            panic!("partial subscription response did not remain in Writing");
        };
        assert!(*offset > 0);
        assert!(*offset < encoded.len());

        client
            .set_nonblocking(true)
            .expect("nonblocking subscription reader");
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.pending.contains_key(&fd) {
            assert!(Instant::now() < deadline, "subscription response stalled");
            let mut chunk = [0_u8; 2048];
            loop {
                match client.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(length) => received.extend_from_slice(&chunk[..length]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("read partial subscription response: {error}"),
                }
            }
            let batch = events
                .wait(Duration::from_millis(100))
                .expect("wait for subscription response write");
            for event in batch {
                if event == crate::platform::Event::StreamWritable(fd) {
                    server
                        .handle_pending_writable(fd, &events, &mut subscribers)
                        .expect("continue subscription response write");
                }
            }
        }
        assert!(!server.pending.contains_key(&fd));
        assert_eq!(subscribers.len(), 1);
        client
            .set_nonblocking(false)
            .expect("blocking subscription final drain");
        BufReader::new(client)
            .read_until(b'\n', &mut received)
            .expect("read initial subscription response");
        assert_eq!(received, encoded);
        let decoded: Response =
            serde_json::from_slice(&received).expect("decode subscription response");
        assert!(decoded.ok);
        drop(server);
        fs::remove_dir_all(root).expect("remove subscription support directory");
    }

    #[test]
    fn pending_handshakes_are_bounded_and_listener_backpressures() {
        let (root, mut server, events) = test_server("pending-cap");
        let clients: Vec<_> = (0..(MAX_PENDING_HANDSHAKES + 4))
            .map(|_| UnixStream::connect(&server.socket_path).expect("connect idle peer"))
            .collect();
        accept_ready(&mut server, &events);
        assert_eq!(server.pending.len(), MAX_PENDING_HANDSHAKES);
        assert!(!server.listener_enabled);

        server
            .maintain(
                &events,
                false,
                Instant::now() + HANDSHAKE_TIMEOUT + Duration::from_millis(1),
            )
            .expect("expire the bounded pending set");
        assert!(server.pending.is_empty());
        assert!(server.listener_enabled);

        let batch = events
            .wait(Duration::from_secs(2))
            .expect("wait for backlogged listener");
        assert!(batch.contains(&crate::platform::Event::Listener));
        server
            .finish_event_batch(&events, true)
            .expect("accept bounded backlog");
        assert!(server.pending.len() <= MAX_PENDING_HANDSHAKES);
        drop(clients);
        drop(server);
        fs::remove_dir_all(root).expect("remove pending-cap support directory");
    }

    #[test]
    fn closed_peer_is_dropped_on_broadcast_and_on_readable() {
        let (agent_side, client) = UnixStream::pair().expect("socket pair");
        agent_side.set_nonblocking(true).expect("nonblocking");
        let fd = agent_side.as_raw_fd();
        let mut subscribers = Subscribers::new();
        subscribers.add(agent_side);

        subscribers.handle_readable(fd);
        assert_eq!(
            subscribers.len(),
            1,
            "spurious readability must keep the subscriber"
        );

        client
            .shutdown(Shutdown::Both)
            .expect("close subscriber peer");
        drop(client);
        subscribers.handle_readable(fd);
        assert_eq!(subscribers.len(), 0, "EOF must drop the subscriber");

        let (agent_side, client) = UnixStream::pair().expect("socket pair");
        agent_side.set_nonblocking(true).expect("nonblocking");
        let mut subscribers = Subscribers::new();
        subscribers.add(agent_side);
        client
            .shutdown(Shutdown::Both)
            .expect("close subscriber peer");
        drop(client);
        subscribers.broadcast(b"{}\n");
        assert_eq!(subscribers.len(), 0, "failed push must drop the subscriber");
    }
}
