//! sd_notify(3), by hand: the daemon tells the systemd user manager it is ready.
//!
//! The shipped user unit is `Type=notify` (ticket 16, packaging/templates/lumen.service.in — the
//! pattern LocalSearch itself uses, research §4.1). Readiness there is one unix datagram to
//! `$NOTIFY_SOCKET`, a protocol small enough to implement directly; the daemon links neither libc's
//! init plumbing nor GLib's `g_application_…`, and adding a libsystemd dependency for one line of
//! text would be the only reason to have one.
//!
//! Outside the notify protocol the daemon's readiness signal is the `ready` line it prints on
//! stdout; both are emitted together (see `main.rs`).
//!
//! With no `NOTIFY_SOCKET` in the environment — a non-systemd session, a terminal, or the test
//! harness — this is a no-op, so nothing here can fail a start.

use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::process;

/// The readiness assignment, in the spelling the notify protocol defines.
pub const READY: &str = "READY=1";

/// The datagram for "up and serving": `READY=1` plus the two lines `systemctl status` shows.
///
/// `status` is flattened to one line: the notify protocol is line-based, so a newline in the
/// status text would be read as a malformed assignment and systemd would reject the datagram.
pub fn notify_payload(status: &str) -> String {
    let status = status.replace(['\n', '\r'], " ");
    format!("{READY}\nSTATUS={status}\nMAINPID={}\n", process::id())
}

/// Send `payload` to the notify socket systemd put in the environment.
///
/// A leading `@` means the abstract namespace (which is what `systemd --user` sets), anything else
/// is a filesystem socket. This is the seam the unit tests drive; `ready()` only reads the env.
pub fn send_to(socket: &str, payload: &str) -> io::Result<()> {
    let address = match socket.strip_prefix('@') {
        Some(name) if !name.is_empty() => SocketAddr::from_abstract_name(name)?,
        Some(_) => return Ok(()),
        None => SocketAddr::from_pathname(socket)?,
    };

    // `send_to` only takes a path; the abstract namespace is what `systemd --user` uses, so the
    // datagram socket is connected to the address instead (one send per readiness announcement).
    let datagram = UnixDatagram::unbound()?;
    datagram.connect_addr(&address)?;
    datagram.send(payload.as_bytes()).map(|_| ())
}

/// Announce readiness to systemd, if systemd asked us to. Never fatal.
pub fn ready() -> io::Result<()> {
    match std::env::var("NOTIFY_SOCKET") {
        Ok(socket) if !socket.is_empty() => send_to(&socket, &notify_payload("serving")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn unique(tag: &str) -> String {
        format!(
            "lumen-notify-{}-{}-{}",
            tag,
            process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        )
    }

    fn receive(socket: &UnixDatagram) -> String {
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a read timeout");
        let mut buffer = [0_u8; 4096];
        let read = socket.recv(&mut buffer).expect("the datagram");
        String::from_utf8_lossy(&buffer[..read]).into_owned()
    }

    #[test]
    fn the_payload_is_ready_with_a_status_and_the_main_pid() {
        let payload = notify_payload("serving the engine");
        assert!(payload.starts_with("READY=1\n"));
        assert!(payload.contains("STATUS=serving the engine\n"));
        assert!(payload.contains(&format!("MAINPID={}\n", process::id())));
        assert!(payload.ends_with('\n'), "the notify protocol is line-based");
    }

    #[test]
    fn a_newline_in_the_status_cannot_break_the_protocol() {
        let payload = notify_payload("one\ntwo\rthree");
        assert_eq!(payload.lines().count(), 3, "{payload}");
        assert!(payload.contains("STATUS=one two three\n"));
    }

    #[test]
    fn a_filesystem_socket_gets_the_datagram() {
        let path = std::env::temp_dir().join(unique("fs.sock"));
        let _ = std::fs::remove_file(&path);
        let listener = UnixDatagram::bind(&path).expect("bind the notify socket");
        let socket = path.to_str().expect("utf-8 path").to_string();

        send_to(&socket, "READY=1\n").expect("send");
        assert_eq!(receive(&listener), "READY=1\n");

        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_abstract_socket_gets_the_datagram_too() {
        // What `systemd --user` actually sets: `@` followed by a name, not a path.
        let name = unique("abstract");
        let address = SocketAddr::from_abstract_name(&name).expect("abstract address");
        let listener = UnixDatagram::bind_addr(&address).expect("bind the abstract socket");

        send_to(&format!("@{name}"), "READY=1\n").expect("send");
        assert_eq!(receive(&listener), "READY=1\n");
    }

    #[test]
    fn a_missing_socket_is_an_error_the_caller_can_ignore() {
        let socket = std::env::temp_dir().join(unique("absent.sock"));
        assert!(send_to(socket.to_str().unwrap(), "READY=1\n").is_err());
    }
}
