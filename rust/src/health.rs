use crate::config::{ValidatedConfig, health_hostname};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: u64 = 32 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthResult {
    pub healthy: bool,
    pub version: String,
}

#[derive(Debug)]
pub enum HealthError {
    Io(io::Error),
    Protocol(String),
    /// HTTP 401: the endpoint is reachable but rejected the Basic-auth
    /// credential. Classified separately because the remedy (re-save the
    /// password in Settings) differs from a generic protocol failure.
    Unauthorized,
}

impl std::fmt::Display for HealthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Unauthorized => {
                formatter.write_str("health endpoint rejected the credential (HTTP 401)")
            }
        }
    }
}

impl std::error::Error for HealthError {}

impl From<io::Error> for HealthError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Deserialize)]
struct HealthBody {
    healthy: bool,
    version: String,
}

pub fn check(config: &ValidatedConfig, timeout: Duration) -> Result<HealthResult, HealthError> {
    check_endpoint(
        health_hostname(&config.source.hostname),
        config.source.port,
        &config.effective_username,
        &config.source.password,
        timeout,
    )
}

pub fn check_endpoint(
    hostname: &str,
    port: u16,
    username: &str,
    password: &str,
    timeout: Duration,
) -> Result<HealthResult, HealthError> {
    let addresses = resolve_addresses(hostname, port)?;
    let started = Instant::now();
    let mut stream = connect_addresses_with(
        &addresses,
        timeout,
        || started.elapsed(),
        TcpStream::connect_timeout,
    )?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    request_health(&mut stream, hostname, port, username, password)
}

fn connect_addresses_with<T>(
    addresses: &[SocketAddr],
    timeout: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut connect: impl FnMut(&SocketAddr, Duration) -> io::Result<T>,
) -> io::Result<T> {
    let mut last_error = None;
    for address in addresses {
        let remaining = timeout.saturating_sub(elapsed()).min(timeout);
        if remaining.is_zero() {
            break;
        }
        match connect(address, remaining) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "no address resolved")))
}

fn resolve_addresses(hostname: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let unbracketed = hostname
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(hostname);
    let addresses: Vec<_> = (unbracketed, port).to_socket_addrs()?.take(8).collect();
    if addresses.is_empty() {
        Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "hostname resolved to no address",
        ))
    } else {
        Ok(addresses)
    }
}

fn request_health(
    stream: &mut TcpStream,
    hostname: &str,
    port: u16,
    username: &str,
    password: &str,
) -> Result<HealthResult, HealthError> {
    let host = if hostname.contains(':') && !hostname.starts_with('[') {
        format!("[{hostname}]:{port}")
    } else {
        format!("{hostname}:{port}")
    };
    let authorization = if password.is_empty() {
        String::new()
    } else {
        let encoded = STANDARD.encode(format!("{username}:{password}"));
        format!("Authorization: Basic {encoded}\r\n")
    };
    let request = format!(
        "GET /global/health HTTP/1.1\r\nHost: {host}\r\n{authorization}Accept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut bytes = Vec::new();
    stream
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(HealthError::Protocol(
            "health response exceeds 32 KiB".to_owned(),
        ));
    }
    parse_response(&bytes)
}

fn parse_response(bytes: &[u8]) -> Result<HealthResult, HealthError> {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| {
            HealthError::Protocol("health response has no header boundary".to_owned())
        })?;
    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| HealthError::Protocol("health response headers are not UTF-8".to_owned()))?;
    let status_line = headers
        .lines()
        .next()
        .ok_or_else(|| HealthError::Protocol("health response is empty".to_owned()))?;
    let status = status_line
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| HealthError::Protocol("health response status is invalid".to_owned()))?;
    if status == 401 {
        return Err(HealthError::Unauthorized);
    }
    if status != 200 {
        return Err(HealthError::Protocol(format!(
            "health endpoint returned HTTP {status}"
        )));
    }
    let body = &bytes[header_end + 4..];
    let parsed: HealthBody = serde_json::from_slice(body)
        .map_err(|error| HealthError::Protocol(format!("health JSON is invalid: {error}")))?;
    if parsed.version.is_empty() || parsed.version.len() > 128 {
        return Err(HealthError::Protocol(
            "health response version is missing or too long".to_owned(),
        ));
    }
    Ok(HealthResult {
        healthy: parsed.healthy,
        version: parsed.version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn parses_expected_health_shape() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"healthy\":true,\"version\":\"1.18.8\"}";
        assert_eq!(
            parse_response(response).expect("health response"),
            HealthResult {
                healthy: true,
                version: "1.18.8".to_owned()
            }
        );
    }

    #[test]
    fn sends_basic_auth_without_exposing_it_in_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("address");
        let open_code_health_fixture = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let length = stream.read(&mut request).expect("read");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("Authorization: Basic dXNlcjpmaXh0dXJlLXZhbHVl"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 35\r\n\r\n{\"healthy\":true,\"version\":\"test\"}",
                )
                .expect("write");
        });
        let result = check_endpoint(
            "127.0.0.1",
            address.port(),
            "user",
            "fixture-value",
            Duration::from_secs(1),
        )
        .expect("health");
        assert!(result.healthy);
        open_code_health_fixture
            .join()
            .expect("OpenCode health fixture");
    }

    #[test]
    fn connection_attempts_share_one_total_timeout() {
        let addresses = [
            "192.0.2.1:4096".parse().expect("first address"),
            "192.0.2.2:4096".parse().expect("second address"),
            "192.0.2.3:4096".parse().expect("third address"),
        ];
        let timeout = Duration::from_millis(100);
        let elapsed = Cell::new(Duration::ZERO);
        let attempts = RefCell::new(Vec::new());

        let error = connect_addresses_with(
            &addresses,
            timeout,
            || elapsed.get(),
            |address, attempt_timeout| {
                attempts.borrow_mut().push((*address, attempt_timeout));
                elapsed.set(match attempts.borrow().len() {
                    1 => Duration::from_millis(40),
                    2 => timeout,
                    _ => panic!("an address was attempted after the total timeout expired"),
                });
                Err::<TcpStream, _>(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "simulated connect timeout",
                ))
            },
        )
        .expect_err("all attempted addresses time out");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            attempts.into_inner(),
            vec![
                (addresses[0], Duration::from_millis(100)),
                (addresses[1], Duration::from_millis(60)),
            ]
        );
    }

    #[test]
    fn rejects_non_success_status() {
        let error = parse_response(b"HTTP/1.1 500 Internal Server Error\r\n\r\n{}")
            .expect_err("must reject")
            .to_string();
        assert_eq!(error, "health endpoint returned HTTP 500");
    }

    #[test]
    fn classifies_401_as_unauthorized() {
        assert!(matches!(
            parse_response(b"HTTP/1.1 401 Unauthorized\r\n\r\n{}").expect_err("must reject"),
            HealthError::Unauthorized
        ));
    }
}
