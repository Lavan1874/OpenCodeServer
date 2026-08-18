//! One-shot Local Network attribution probe for a diagnostic build only.
//!
//! This module is deliberately feature-gated and is not part of a normal
//! product build. It lets the OpenCodeServerAgent itself perform a minimal
//! multicast operation before any external OpenCode child exists, separating
//! the agent-to-app attribution question from the external-child question.

use crate::platform::{LogLevel, log};
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::thread;
use std::time::Duration;

const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;
const PROBE_DURATION: Duration = Duration::from_millis(300);

/// Start the bounded probe without delaying supervisor startup or IPC.
pub fn start_once() {
    let spawn_result = thread::Builder::new()
        .name("local-network-probe".to_owned())
        .spawn(|| match probe() {
            Ok(()) => log(LogLevel::Notice, "Diagnostic Local Network probe completed"),
            Err(error) => log(
                LogLevel::Error,
                &format!("Diagnostic Local Network probe failed: {error}"),
            ),
        });

    if let Err(error) = spawn_result {
        log(
            LogLevel::Error,
            &format!("Unable to start diagnostic Local Network probe: {error}"),
        );
    }
}

fn probe() -> io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_multicast_loop_v4(false)?;
    socket.join_multicast_v4(&MDNS_GROUP, &Ipv4Addr::UNSPECIFIED)?;
    // Joining the group alone may not emit traffic. Send a bounded, harmless
    // DNS-like query so securityd observes an actual multicast operation.
    // The payload is intentionally not a real service query and no response
    // is read or retained.
    socket.send_to(&[0, 0, 0, 0], (MDNS_GROUP, MDNS_PORT))?;
    thread::sleep(PROBE_DURATION);
    socket.leave_multicast_v4(&MDNS_GROUP, &Ipv4Addr::UNSPECIFIED)
}
