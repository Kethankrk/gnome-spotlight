//! The clipboard key in a **real** login keyring, over the real Secret Service.
//!
//! Ticket 11's own suite runs without a keyring (that is what `SecretKeySource` is for), so this
//! test exists to keep the real D-Bus client honest: it starts `gnome-keyring-daemon` on the
//! ambient session bus with a throw-away `HOME`, drives two daemons through a restart, and checks
//! that the key really is a keyring item and the history really is encrypted on disk under it.
//!
//! It is ignored by default because it needs `gnome-keyring-daemon` on the host and a bus of its
//! own. Run it with:
//!
//! ```text
//! dbus-run-session -- cargo test --test keyring -- --ignored --nocapture
//! ```
//!
//! Verified on the build host with gnome-keyring 50.0 (2026-11-01): the key is created in the
//! default (login) collection, found again after a restart, and the history file holds none of the
//! text in the clear.

mod support;

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use support::{block, connect, engine_proxy, name_is_owned, Harness};

/// The password of the throw-away keyring. Nothing protects it: it protects nothing.
const PASSWORD: &str = "lumen-test-password";
/// The service the keyring owns.
const SECRETS: &str = "org.freedesktop.secrets";

/// A throw-away `HOME` and XDG tree, so neither the developer's keyring nor their data directory
/// is touched.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lumen-keyring-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("run")).expect("a scratch runtime directory");
        Self { root }
    }

    /// The environment the daemons and the keyring run with, as `(&str, &str)` for the harness.
    fn env(&self) -> Vec<(String, String)> {
        let home = self.root.to_str().expect("a utf-8 path");
        vec![
            ("HOME".to_string(), home.to_string()),
            ("XDG_DATA_HOME".to_string(), format!("{home}/data")),
            ("XDG_CONFIG_HOME".to_string(), format!("{home}/config")),
            ("XDG_STATE_HOME".to_string(), format!("{home}/state")),
            ("XDG_CACHE_HOME".to_string(), format!("{home}/cache")),
            ("XDG_RUNTIME_DIR".to_string(), format!("{home}/run")),
        ]
    }

    fn spawn(&self, extra: &[&str]) -> Harness {
        let owned = self.env();
        let env: Vec<(&str, &str)> = owned
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        Harness::spawn_with_env(extra, &env)
    }

    /// The history file, if it exists.
    fn history(&self) -> Option<Vec<u8>> {
        std::fs::read(self.root.join("data/lumen/clipboard.bin")).ok()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Where the bus is: the harness reuses an existing one, and the keyring has to be on the same bus.
fn session_address() -> String {
    std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .expect("run this test under `dbus-run-session -- cargo test --test keyring -- --ignored`")
}

/// Start a keyring whose files live in `scratch`, and wait until it owns `org.freedesktop.secrets`.
///
/// A `--unlock` with a fresh `HOME` creates the login keyring and leaves it unlocked, which is the
/// state a user's keyring is in after login.
fn start_keyring(scratch: &Scratch, address: &str) {
    let mut keyring = Command::new("gnome-keyring-daemon")
        .args(["--unlock", "--components=secrets", "--replace"])
        .env("HOME", &scratch.root)
        .env("XDG_RUNTIME_DIR", scratch.root.join("run"))
        .env("DBUS_SESSION_BUS_ADDRESS", address)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("gnome-keyring-daemon must be installed for this test");

    {
        let mut stdin = keyring.stdin.take().expect("the keyring's stdin");
        stdin
            .write_all(PASSWORD.as_bytes())
            .expect("the keyring's password");
    }
    // The daemon forks into the background once the password has been read.
    let _ = keyring.wait();

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let owned = block({
            let address = address.to_string();
            async move {
                let connection = connect(&address).await.expect("connect to the bus");
                name_is_owned(&connection, SECRETS)
                    .await
                    .expect("ask the bus about the keyring")
            }
        });
        if owned {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("gnome-keyring-daemon never owned {SECRETS}");
}

/// The keyring items the store's key is found by: `SearchItems` on the attributes it creates with.
fn keyring_items(address: &str) -> Vec<String> {
    block({
        let address = address.to_string();
        async move {
            let connection = connect(&address).await.expect("connect to the bus");
            let service = zbus::Proxy::new(
                &connection,
                SECRETS,
                "/org/freedesktop/secrets",
                "org.freedesktop.Secret.Service",
            )
            .await
            .expect("the secret service proxy");
            let attributes: HashMap<String, String> = [
                ("application".to_string(), "lumen".to_string()),
                ("purpose".to_string(), "clipboard-history".to_string()),
            ]
            .into_iter()
            .collect();

            let (unlocked, locked): (Vec<zbus::OwnedObjectPath>, Vec<zbus::OwnedObjectPath>) = service
                .call("SearchItems", &(attributes,))
                .await
                .expect("SearchItems");
            unlocked
                .into_iter()
                .chain(locked)
                .map(|path| path.to_string())
                .collect()
        }
    })
}

#[test]
#[ignore = "needs gnome-keyring-daemon; run under dbus-run-session with --ignored"]
fn the_key_lives_in_the_login_keyring_and_the_history_reloads_from_it() {
    let scratch = Scratch::new();
    let address = session_address();
    // Before any daemon: it opens its store (and so its key) at start-up.
    start_keyring(&scratch, &address);

    // ---- a first daemon: the key is created, the history is written encrypted ----
    let mut harness = scratch.spawn(&["--clipboard", "persist"]);
    let name = harness.name().to_string();

    let listed = block({
        let address = address.clone();
        async move {
            let connection = connect(&address).await.expect("connect to the bus");
            let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
            proxy
                .call::<_, _, ()>("ClipboardSetEnabled", &(true,))
                .await
                .expect("ClipboardSetEnabled");
            let recorded: bool = proxy
                .call("ClipboardPush", &("kept in the keyring", 1_000u64))
                .await
                .expect("ClipboardPush");
            assert!(recorded, "the copy is recorded");
            proxy
                .call::<_, _, Vec<HashMap<String, zbus::OwnedVariant>>>("ClipboardList", &())
                .await
                .expect("ClipboardList")
        }
    });

    assert_eq!(listed.len(), 1, "the copy is listed: {listed:?}");
    assert!(harness.terminate().success());

    let items = keyring_items(&address);
    assert_eq!(
        items.len(),
        1,
        "the daemon's key must be an item in the login keyring, found {items:?}"
    );

    let sealed = scratch.history().expect("the history file was written");
    assert!(
        !contains(&sealed, "kept in the keyring"),
        "the history file must not hold the text in the clear"
    );

    // ---- a second daemon: it reads the key back from the keyring, and the history with it ----
    let mut harness = scratch.spawn(&["--clipboard", "persist"]);
    let name = harness.name().to_string();

    let (enabled, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let enabled: bool = proxy
            .get_property("ClipboardEnabled")
            .await
            .expect("ClipboardEnabled");
        let rows: Vec<HashMap<String, zbus::OwnedVariant>> =
            proxy.call("ClipboardList", &()).await.expect("ClipboardList");
        (enabled, rows)
    });

    assert!(enabled, "history comes back on");
    assert_eq!(
        rows.len(),
        1,
        "the history file must decrypt under the keyring's key: {rows:?}"
    );
    assert!(harness.terminate().success());
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}
