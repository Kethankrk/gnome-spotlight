//! Test harness for the daemon's D-Bus seam.
//!
//! Every integration test drives the real `lumen-daemon` binary as a D-Bus client over a private
//! session bus. When `DBUS_SESSION_BUS_ADDRESS` is already set (as under
//! `dbus-run-session -- cargo test`) that bus is reused and the daemon is started under a unique
//! well-known name; otherwise the harness starts a `dbus-daemon` of its own.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use zbus::names::BusName;
use zbus::Proxy;

pub const OBJECT_PATH: &str = "/org/gnome/Lumen";
pub const INTERFACE: &str = "org.gnome.Lumen.Engine";

/// What the daemon opens a uri with unless a test names its own program: `LUMEN_OPEN_BIN` is the
/// opener's seam (contract §19), and a suite must not launch the developer's applications.
pub const DEFAULT_TEST_OPEN_BIN: &str = "/bin/true";

const READY_TIMEOUT: Duration = Duration::from_secs(20);
const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct Harness {
    address: String,
    name: String,
    daemon: Child,
    bus: Option<Child>,
}

impl Harness {
    /// A daemon on a private bus (or the ambient one) under a name unique to this test.
    pub fn spawn() -> Self {
        Self::spawn_with_args(&[])
    }

    pub fn spawn_with_args(extra: &[&str]) -> Self {
        Self::start(extra, true, &[])
    }

    /// Extra arguments and extra environment (e.g. a throw-away `HOME`) together, under a unique
    /// test name.
    pub fn spawn_with_env(extra: &[&str], env: &[(&str, &str)]) -> Self {
        Self::start(extra, true, env)
    }

    /// A daemon owning the real well-known name, always on a bus of its own so that it cannot
    /// race another test for it.
    pub fn spawn_with_default_name() -> Self {
        Self::start(&[], false, &[])
    }

    fn start(extra: &[&str], unique_name: bool, env: &[(&str, &str)]) -> Self {
        let (bus, address) = if unique_name {
            match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
                Ok(address) => (None, address),
                Err(_) => {
                    let (child, address) = start_bus();
                    (Some(child), address)
                }
            }
        } else {
            let (child, address) = start_bus();
            (Some(child), address)
        };

        let name = if unique_name {
            format!(
                "org.gnome.Lumen.Test.T{}_{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            )
        } else {
            "org.gnome.Lumen".to_string()
        };

        let mut command = Command::new(env!("CARGO_BIN_EXE_lumen-daemon"));
        command
            .arg("--object-path")
            .arg(OBJECT_PATH)
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            // The daemon opens a `file:` row with the desktop's default application. A test suite
            // must never launch the developer's applications, so unless a test names its own
            // program the opener is `/bin/true` (contract §19; see `daemon/tests/activation.rs`
            // for the tests that watch what is opened).
            .env("LUMEN_OPEN_BIN", DEFAULT_TEST_OPEN_BIN)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if unique_name {
            command.arg("--dbus-name").arg(&name);
        }
        // Hermetic by default: unless a test asks for the persistent store, the daemon keeps the
        // history in memory. Otherwise a run on a developer's own session bus would write to their
        // data directory and their keyring (ticket 11 makes persistence the default).
        if !extra
            .iter()
            .any(|arg| *arg == "--clipboard" || arg.starts_with("--clipboard="))
        {
            command.args(["--clipboard", "memory"]);
        }
        command.args(extra);

        let mut daemon = command.spawn().expect("spawn lumen-daemon");
        wait_for_ready(&mut daemon, &name);

        Self {
            address,
            name,
            daemon,
            bus,
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pid(&self) -> u32 {
        self.daemon.id()
    }

    /// Ask the daemon to shut down; the process must exit successfully.
    pub fn terminate(&mut self) -> ExitStatus {
        unsafe {
            libc::kill(self.daemon.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            match self.daemon.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Ok(None) => panic!("lumen-daemon did not exit after SIGTERM"),
                Err(e) => panic!("waiting for lumen-daemon failed: {e}"),
            }
        }
    }

    pub fn stderr(&mut self) -> String {
        use std::io::Read;
        let mut out = String::new();
        if let Some(mut err) = self.daemon.stderr.take() {
            let _ = err.read_to_string(&mut out);
        }
        out
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        if let Some(mut bus) = self.bus.take() {
            let _ = bus.kill();
            let _ = bus.wait();
        }
    }
}

fn start_bus() -> (Child, String) {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--print-address=1", "--nofork"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a private session bus with dbus-daemon");

    let stdout = child.stdout.take().expect("dbus-daemon address on stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("read the private bus address");
    let address = line.trim().to_string();
    assert!(
        !address.is_empty(),
        "dbus-daemon printed no session bus address"
    );
    (child, address)
}

fn wait_for_ready(daemon: &mut Child, name: &str) {
    let stdout = daemon.stdout.take().expect("daemon stdout");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let read = reader.read_line(&mut line);
        let _ = tx.send((read, line));
    });

    match rx.recv_timeout(READY_TIMEOUT) {
        Ok((Ok(n), line)) if n > 0 && line.contains("ready") => {}
        Ok((_, line)) => {
            panic!("daemon for {name} stopped before becoming ready (last output: {line:?})")
        }
        Err(e) => panic!("daemon for {name} was not ready in time: {e}"),
    }
}

/// Connect to a bus by address.
pub async fn connect(address: &str) -> zbus::Result<zbus::Connection> {
    zbus::connection::Builder::address(address).build().await
}

pub async fn engine_proxy<'a>(
    connection: &'a zbus::Connection,
    name: &'a str,
) -> zbus::Result<Proxy<'a>> {
    Proxy::new(connection, name, OBJECT_PATH, INTERFACE).await
}

pub async fn name_is_owned(connection: &zbus::Connection, name: &str) -> zbus::fdo::Result<bool> {
    let bus = zbus::fdo::DBusProxy::new(connection).await?;
    bus.name_has_owner(BusName::try_from(name).expect("well-known bus name"))
        .await
}

/// Run a future on a thread of its own and fail the test if it does not finish in time.
pub fn block<T: Send + 'static, F: std::future::Future<Output = T> + Send + 'static>(
    future: F,
) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let out = zbus::block_on(future);
        let _ = tx.send(out);
    });
    rx.recv_timeout(CALL_TIMEOUT)
        .expect("the D-Bus client did not finish in time")
}

/// A caller-built result row: the shape `Query.options["injected"]` carries (contract §2).
pub fn injected(
    id: &str,
    provider: &str,
    title: &str,
) -> HashMap<String, zbus::OwnedVariant> {
    let ov = |value: &str| {
        zbus::OwnedVariant::try_from(zbus::Variant::from(value.to_string()))
            .expect("an owned string")
    };
    HashMap::from([
        ("id".to_string(), ov(id)),
        ("provider".to_string(), ov(provider)),
        ("title".to_string(), ov(title)),
    ])
}

/// `options` with an `injected` `aa{sv}` of caller-computed candidates.
pub fn injected_options(
    rows: Vec<HashMap<String, zbus::OwnedVariant>>,
) -> HashMap<String, zbus::OwnedVariant> {
    let array: Vec<zbus::OwnedVariant> = rows.into_iter().map(zbus::OwnedVariant::from).collect();
    let mut options = HashMap::new();
    options.insert(
        "injected".to_string(),
        zbus::OwnedVariant::try_from(zbus::Variant::from(array)).expect("an owned a{sv} array"),
    );
    options
}
