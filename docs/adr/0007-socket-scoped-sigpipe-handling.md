# ADR 0007: Socket-scoped SIGPIPE handling

## Status

Accepted on 2026-07-30.

## Context

OpenCodeServer polls `OpenCodeServerAgent` over an `AF_UNIX`
`SOCK_STREAM` socket. If OpenCodeServerAgent closes or restarts while
OpenCodeServer is
writing a request, Darwin can fail the write with `EPIPE` and deliver
`SIGPIPE` to the calling thread. The default signal disposition terminates the
application before Swift can turn the failed write into a recoverable IPC
error.

The Build 4 OpenCodeServer process exited this way while the independently
managed OpenCodeServerAgent and OpenCode remained healthy.

Apple documents both this `send(2)` behavior and the socket-level
`SO_NOSIGPIPE` option:

- [send(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/send.2.html)
- [setsockopt(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/setsockopt.2.html)
- [Using Sockets and Socket Streams](https://developer.apple.com/library/archive/documentation/NetworkingInternet/Conceptual/NetworkingTopics/Articles/UsingSocketsandSocketStreams.html)
- [Avoiding Common Networking Mistakes](https://developer.apple.com/library/archive/documentation/NetworkingInternetWeb/Conceptual/NetworkingOverview/CommonPitfalls/CommonPitfalls.html)

The macOS 26.5 SDK also defines `SO_NOSIGPIPE` as “No SIGPIPE on EPIPE” in
`usr/include/sys/socket.h`.

## Decision

Every Swift IPC client socket sets `SO_NOSIGPIPE` immediately after creation
and treats failure to apply the option as a connection setup failure. Failed
writes then return `EPIPE` through the existing error path instead of
terminating the menu bar process.

Receive and send timeouts are also checked rather than silently ignoring
`setsockopt` failures. An orderly close without a response is reported as a
normal, recoverable IPC error. The next periodic poll creates a new socket and
can recover when OpenCodeServerAgent becomes reachable again.

The process-wide `SIGPIPE` disposition is unchanged. This keeps the exception
scoped to the Unix socket whose peer lifecycle is expected to be independent.

## Consequences

- OpenCodeServerAgent restarts, timeouts, and connection shutdown races cannot
  terminate OpenCodeServer through this socket.
- IPC failures continue to produce an unavailable OpenCodeServer state and do
  not signal OpenCodeServerAgent or OpenCode.
- Any future socket creation path must explicitly choose and test its SIGPIPE
  behavior instead of inheriting a hidden process-global policy.
