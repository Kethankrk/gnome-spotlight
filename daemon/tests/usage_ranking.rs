//! Usage-based ranking at the D-Bus seam: the real daemon binary, a real session bus, a real
//! client.
//!
//! The daemon learns what the user reaches for from `Activate`. The counters feed contract §5's
//! `usage_bonus` on the next query, they are preferred over a weaker match but never over an exact
//! one, and they live in a file under the user's data directory so a restart keeps them.
//!
//! Every test here gives the daemon a throw-away `HOME`/`XDG_DATA_HOME`, so the counters land in a
//! scratch directory rather than in the developer's own.

mod support;

use std::collections::HashMap;
use std::path::PathBuf;

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, injected, injected_options, Harness};

/// `a{sv}`: one result-schema row, or the `Activate` outcome.
type Row = HashMap<String, zbus::OwnedVariant>;
/// `aa{sv}`: the results of one search.
type Rows = Vec<Row>;

fn member(row: &Row, key: &str) -> zbus::Variant<'static> {
    zbus::Variant::from(row[key].try_clone().expect("cloneable"))
}

fn text_of(row: &Row, key: &str) -> String {
    String::try_from(&member(row, key)).expect("a string")
}

fn bool_of(row: &Row, key: &str) -> bool {
    bool::try_from(&member(row, key)).expect("a boolean")
}

fn usage_of(row: &Row, key: &str) -> u32 {
    u32::try_from(&member(row, key)).expect("a number")
}

fn number_of(row: &Row, key: &str) -> f64 {
    f64::try_from(&member(row, key)).expect("a number")
}

fn providers(rows: &Rows) -> Vec<String> {
    rows.iter().map(|row| text_of(row, "provider")).collect()
}

/// The `u` variant of a 32-bit count, so the caller can build an options map.
fn options(rows: Vec<Row>) -> HashMap<String, zbus::OwnedVariant> {
    injected_options(rows)
}

/// One `Query`, returning the results of the single `ResultsChanged` that answers it.
async fn query(
    proxy: &zbus::Proxy<'_>,
    text: &str,
    providers: &[&str],
    options: HashMap<String, zbus::OwnedVariant>,
) -> Rows {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let providers: Vec<String> = providers.iter().map(|id| (*id).to_string()).collect();
    let id: String = proxy
        .call("Query", &(text, providers, options))
        .await
        .expect("Query");
    assert!(!id.is_empty());

    let signal = changed.next().await.expect("ResultsChanged signal");
    let (signal_id, results): (String, Rows) = signal.body().deserialize().expect("body");
    assert_eq!(signal_id, id);
    results
}

/// One `Activate`, exactly as the extension calls it.
async fn activate(proxy: &zbus::Proxy<'_>, result_id: &str, action_id: &str) -> Row {
    proxy
        .call("Activate", &(result_id, action_id))
        .await
        .expect("Activate")
}

/// A throw-away data tree for one test, so counters never land in the developer's own.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "lumen-usage-dbus-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("a scratch data directory");
        Self(root)
    }

    fn data_home(&self) -> String {
        self.0.to_str().expect("a utf-8 path").to_string()
    }

    /// The file the daemon's counters live in.
    fn counters(&self) -> PathBuf {
        self.0.join("lumen/usage.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A daemon whose counters are written into `scratch`.
fn daemon(scratch: &Scratch) -> Harness {
    let data_home = scratch.data_home();
    Harness::spawn_with_env(
        &[],
        &[
            ("HOME", &data_home),
            ("XDG_DATA_HOME", &data_home),
            ("XDG_STATE_HOME", &data_home),
            ("XDG_CONFIG_HOME", &data_home),
        ],
    )
}

/// An application candidate and a file candidate that both match `note` as a prefix, the app
/// ahead of the file because applications carry a heavier base weight (contract §5).
fn notes_candidates() -> Vec<Row> {
    vec![
        injected("app:org.gnome.Notes.desktop", "applications", "Notes"),
        injected("file:/home/me/notes-old", "files", "notes-old"),
    ]
}

#[test]
fn repeated_activation_promotes_a_result_in_the_ranking() {
    let scratch = Scratch::new("promote");
    let harness = daemon(&scratch);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (before, first, last, after) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let providers = ["applications", "files"];

        let before = query(&proxy, "note", &providers, options(notes_candidates())).await;

        let first = activate(&proxy, "file:/home/me/notes-old", "open").await;
        let mut last = first.clone();
        for _ in 1..20 {
            last = activate(&proxy, "file:/home/me/notes-old", "open").await;
        }

        let after = query(&proxy, "note", &providers, options(notes_candidates())).await;
        (before, first, last, after)
    });

    assert_eq!(
        providers(&before),
        vec!["applications".to_string(), "files".to_string()],
        "before any activation the app's heavier base weight wins"
    );

    assert_eq!(
        usage_of(&first, "usage"),
        1,
        "Activate reports the new count of the result it recorded"
    );
    assert_eq!(
        usage_of(&last, "usage"),
        20,
        "and it climbs with every press"
    );
    assert!(
        bool_of(&last, "handled"),
        "the daemon opens the file itself (contract §2, §19); the harness points the opener at \
         /bin/true, so nothing is actually launched"
    );

    assert_eq!(
        providers(&after),
        vec!["files".to_string(), "applications".to_string()],
        "twenty activations of the file promote it past the app"
    );
    let file = after[0].clone();
    let app = after[1].clone();
    assert_eq!(text_of(&file, "id"), "file:/home/me/notes-old");
    assert!(
        (number_of(&file, "score") - 1.20).abs() < 1e-9,
        "files 0.20 + prefix 0.8 + usage 0.20, got {}",
        number_of(&file, "score")
    );
    assert!(
        (number_of(&app, "score") - 1.10).abs() < 1e-9,
        "applications 0.30 + prefix 0.8, got {}",
        number_of(&app, "score")
    );
}

#[test]
fn an_exact_match_still_outranks_a_frequently_used_loose_match() {
    let scratch = Scratch::new("exact");
    let harness = daemon(&scratch);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (before, after) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let providers = ["applications", "files"];
        let candidates = || {
            options(vec![
                injected("app:org.gnome.Nautilus.desktop", "applications", "Files"),
                injected("file:/home/me/My Files Backup", "files", "My Files Backup"),
            ])
        };

        let before = query(&proxy, "files", &providers, candidates()).await;
        // Far past the scorer's cap of 20 activations.
        for _ in 0..40 {
            activate(&proxy, "file:/home/me/My Files Backup", "open").await;
        }
        let after = query(&proxy, "files", &providers, candidates()).await;
        (before, after)
    });

    assert_eq!(
        providers(&before),
        vec!["applications".to_string(), "files".to_string()]
    );
    assert_eq!(
        providers(&after),
        vec!["applications".to_string(), "files".to_string()],
        "frequency must never outrank an exact application match"
    );

    let app = after[0].clone();
    let file = after[1].clone();
    assert_eq!(text_of(&app, "title"), "Files");
    assert!(
        (number_of(&app, "score") - 1.30).abs() < 1e-9,
        "an exact app match is still 0.30 + 1.0, got {}",
        number_of(&app, "score")
    );
    assert!(
        (number_of(&file, "score") - 1.00).abs() < 1e-9,
        "a word-start file match at the usage cap is 0.20 + 0.6 + 0.20, got {}",
        number_of(&file, "score")
    );
}

#[test]
fn learned_ranking_survives_a_restart() {
    let scratch = Scratch::new("restart");

    let mut first = daemon(&scratch);
    let address = first.address().to_string();
    let name = first.name().to_string();

    let before = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let providers = ["applications", "files"];

        let before = query(&proxy, "note", &providers, options(notes_candidates())).await;
        for _ in 0..20 {
            activate(&proxy, "file:/home/me/notes-old", "open").await;
        }
        before
    });
    assert_eq!(
        providers(&before),
        vec!["applications".to_string(), "files".to_string()]
    );

    let status = first.terminate();
    assert!(status.success(), "SIGTERM must be a clean exit: {status}");
    drop(first);

    assert!(
        scratch.counters().is_file(),
        "the counters must live under the user's data directory, looked in {:?}",
        scratch.counters()
    );

    // A brand new daemon, the same data directory: it has to remember.
    let second = daemon(&scratch);
    let address = second.address().to_string();
    let name = second.name().to_string();

    let after = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        query(
            &proxy,
            "note",
            &["applications", "files"],
            options(notes_candidates()),
        )
        .await
    });

    assert_eq!(
        providers(&after),
        vec!["files".to_string(), "applications".to_string()],
        "what the first daemon learned must survive into the second"
    );
    assert_eq!(text_of(&after[0], "id"), "file:/home/me/notes-old");
    assert!(
        (number_of(&after[0], "score") - 1.20).abs() < 1e-9,
        "the count came back whole, got {}",
        number_of(&after[0], "score")
    );
}
