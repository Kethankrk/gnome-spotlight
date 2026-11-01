//! Clipboard history at the D-Bus seam: the real daemon binary, a real session bus, a real client.
//!
//! History is off until the user turns it on. While it is off nothing is recorded, `ClipboardList`
//! is empty, and the results carry one row that says so instead of an error.
//!
//! The history is persisted (ticket 11) as soon as it changes, encrypted under a key held outside
//! the data directory, so the last three tests here run the daemon with `--clipboard persist`,
//! their own throw-away `HOME`/XDG tree and their own key file. Every other test runs with
//! `--clipboard memory` — the harness's default — so a run on a developer's session bus cannot
//! reach their data directory or their keyring.

mod support;

use std::collections::HashMap;

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, Harness};

/// `a{sv}`: one result-schema row.
type Row = HashMap<String, zbus::OwnedVariant>;
/// `aa{sv}`: the `ClipboardList` reply, or the results of one search.
type Rows = Vec<Row>;

/// One `a{sv}` member, as the `zbus::Variant` zvariant converts from.
fn member(row: &Row, key: &str) -> zbus::Variant<'static> {
    zbus::Variant::from(row[key].try_clone().expect("cloneable"))
}

fn text_of(row: &Row, key: &str) -> String {
    String::try_from(&member(row, key)).expect("a string")
}

fn preview_of(row: &Row) -> Row {
    HashMap::try_from(member(row, "preview")).expect("the preview must be an a{sv}")
}

fn titles(rows: &Rows) -> Vec<String> {
    rows.iter().map(|row| text_of(row, "title")).collect()
}

fn ids(rows: &Rows) -> Vec<String> {
    rows.iter().map(|row| text_of(row, "id")).collect()
}

fn actions_of(row: &Row) -> Vec<Row> {
    Vec::try_from(member(row, "actions")).expect("actions must be aa{sv}")
}

async fn list(proxy: &zbus::Proxy<'_>) -> Rows {
    proxy.call("ClipboardList", &()).await.expect("ClipboardList")
}

async fn push(proxy: &zbus::Proxy<'_>, text: &str, timestamp_ms: u64) -> bool {
    proxy
        .call("ClipboardPush", &(text, timestamp_ms))
        .await
        .expect("ClipboardPush")
}

async fn delete(proxy: &zbus::Proxy<'_>, id: &str) -> bool {
    proxy
        .call("ClipboardDelete", &(id,))
        .await
        .expect("ClipboardDelete")
}

async fn set_enabled(proxy: &zbus::Proxy<'_>, enabled: bool) {
    proxy
        .call::<_, _, ()>("ClipboardSetEnabled", &(enabled,))
        .await
        .expect("ClipboardSetEnabled")
}

async fn clear(proxy: &zbus::Proxy<'_>) {
    proxy
        .call::<_, _, ()>("ClipboardClear", &())
        .await
        .expect("ClipboardClear")
}

/// One `Query`, returning the results of the single `ResultsChanged` that answers it.
async fn query(proxy: &zbus::Proxy<'_>, text: &str, providers: &[&str]) -> Rows {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let providers: Vec<String> = providers.iter().map(|id| (*id).to_string()).collect();
    let id: String = proxy
        .call(
            "Query",
            &(
                text,
                providers,
                HashMap::<String, zbus::OwnedVariant>::new(),
            ),
        )
        .await
        .expect("Query");
    assert!(!id.is_empty());

    let signal = changed.next().await.expect("ResultsChanged signal");
    let (signal_id, results): (String, Rows) =
        signal.body().deserialize().expect("ResultsChanged body");
    assert_eq!(signal_id, id);
    results
}

#[test]
fn history_is_off_by_default_and_lists_nothing() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (enabled, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let enabled: bool = proxy
            .get_property("ClipboardEnabled")
            .await
            .expect("ClipboardEnabled");
        (enabled, list(&proxy).await)
    });

    assert!(
        !enabled,
        "clipboard history must be off until the user turns it on"
    );
    assert!(rows.is_empty(), "an off history lists nothing, got {rows:?}");
}

#[test]
fn a_push_is_ignored_while_history_is_off() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (recorded, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let recorded = push(&proxy, "never recorded", 1_000).await;
        (recorded, list(&proxy).await)
    });

    assert!(!recorded, "a push while history is off must not be recorded");
    assert!(rows.is_empty(), "nothing may be recorded while off");
}

#[test]
fn a_query_reports_that_history_is_off_instead_of_an_error() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (rows, again) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let rows = query(&proxy, "anything", &["clipboard"]).await;
        // A disabled row is not an error, so the engine keeps answering.
        let again = query(&proxy, "anything else", &["clipboard"]).await;
        (rows, again)
    });

    assert_eq!(
        ids(&rows),
        vec!["clip:disabled".to_string()],
        "the clipboard section carries exactly one row while history is off"
    );
    assert_eq!(
        text_of(&rows[0], "provider"),
        "clipboard",
        "the disabled row belongs to the clipboard provider"
    );
    assert!(!text_of(&rows[0], "title").is_empty());
    assert_eq!(again.len(), 1, "and it repeats for the next query: {again:?}");
}

#[test]
fn enabling_then_pushing_lists_entries_newest_first() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (property, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");

        set_enabled(&proxy, true).await;
        let property: bool = proxy
            .get_property("ClipboardEnabled")
            .await
            .expect("ClipboardEnabled");

        assert!(push(&proxy, "the first copy", 1_000).await);
        assert!(push(&proxy, "the second copy", 2_000).await);
        (property, list(&proxy).await)
    });

    assert!(property, "ClipboardSetEnabled must show through the property");
    assert_eq!(
        titles(&rows),
        vec!["the second copy".to_string(), "the first copy".to_string()],
        "entries are listed newest first"
    );

    for id in ids(&rows) {
        assert!(
            id.starts_with("clip:"),
            "clipboard ids carry contract §3's `clip:` prefix, got {id}"
        );
    }
    assert_ne!(
        text_of(&rows[0], "id"),
        text_of(&rows[1], "id"),
        "different text must not collide"
    );

    let preview = preview_of(&rows[0]);
    assert_eq!(text_of(&preview, "kind"), "clipboard");
    assert_eq!(text_of(&preview, "text"), "the second copy");
    assert_eq!(
        u64::try_from(&member(&preview, "timestamp")).expect("a t"),
        2_000
    );
}

#[test]
fn copying_the_same_text_again_moves_it_to_the_front_without_duplicating_it() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;

        push(&proxy, "same", 1_000).await;
        push(&proxy, "other", 1_500).await;
        push(&proxy, "same", 2_000).await;
        list(&proxy).await
    });

    assert_eq!(
        titles(&rows),
        vec!["same".to_string(), "other".to_string()],
        "history is de-duplicated and the newest copy wins"
    );
}

#[test]
fn pushing_an_empty_text_records_nothing() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (recorded, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;
        let recorded = push(&proxy, "", 1_000).await;
        (recorded, list(&proxy).await)
    });

    assert!(!recorded, "there is no entry in an empty copy");
    assert!(rows.is_empty());
}

#[test]
fn a_single_entry_can_be_deleted_and_the_whole_history_cleared() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (missing, after_delete, after_clear) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;

        push(&proxy, "one", 1_000).await;
        push(&proxy, "two", 2_000).await;
        push(&proxy, "three", 3_000).await;

        let victim = text_of(&list(&proxy).await[1], "id");
        assert!(delete(&proxy, &victim).await, "the entry existed");
        let after_delete = list(&proxy).await;

        let missing = delete(&proxy, "clip:0000000000000000").await;

        clear(&proxy).await;
        let after_clear = list(&proxy).await;
        (missing, after_delete, after_clear)
    });

    assert_eq!(
        titles(&after_delete),
        vec!["three".to_string(), "one".to_string()],
        "only the deleted entry goes"
    );
    assert!(!missing, "deleting an id that is not there reports false");
    assert!(after_clear.is_empty(), "ClipboardClear empties the history");
}

#[test]
fn clipboard_entries_are_matched_by_the_scorer_and_carry_a_copy_and_a_delete_action() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (matching, unrelated) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;

        push(&proxy, "https://example.com/pricing", 1_000).await;
        push(&proxy, "an unrelated note", 2_000).await;

        let matching = query(&proxy, "example", &["clipboard"]).await;
        let unrelated = query(&proxy, "zzzznotpresent", &["clipboard"]).await;
        (matching, unrelated)
    });

    assert_eq!(
        titles(&matching),
        vec!["https://example.com/pricing".to_string()],
        "only the entry matching the query is a result"
    );

    let actions = actions_of(&matching[0]);
    let described: Vec<(String, String)> = actions
        .iter()
        .map(|action| (text_of(action, "id"), text_of(action, "handler")))
        .collect();
    assert_eq!(
        described,
        vec![
            ("copy".to_string(), "copy".to_string()),
            ("delete".to_string(), "shell".to_string()),
        ],
        "an entry can be copied back and forgotten (stories 46 and 48)"
    );

    assert!(
        unrelated.is_empty(),
        "with history on, a query that matches nothing yields nothing: {unrelated:?}"
    );
}

#[test]
fn clipboard_changed_announces_every_change_of_the_history() {
    let harness = Harness::spawn();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let changes = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let mut changed = proxy
            .receive_signal("ClipboardChanged")
            .await
            .expect("ClipboardChanged stream");

        set_enabled(&proxy, true).await;
        changed.next().await.expect("enabling is a change");

        push(&proxy, "watched", 1_000).await;
        changed.next().await.expect("a push is a change");

        let id = text_of(&list(&proxy).await[0], "id");
        delete(&proxy, &id).await;
        changed.next().await.expect("a delete is a change");

        push(&proxy, "watched again", 2_000).await;
        changed.next().await.expect("the second push is a change");

        clear(&proxy).await;
        changed.next().await.expect("a clear is a change");
    });

    let _ = changes;
}

#[test]
fn the_history_is_encrypted_on_disk_and_survives_a_restart() {
    let scratch = Scratch::new("restart");

    // ---- a first daemon: record two copies, and try to record a secret ----
    let (listed, refused, clean_exit) = {
        let mut harness = scratch.spawn(&["--clipboard", "persist"]);
        let address = harness.address().to_string();
        let name = harness.name().to_string();

        let (listed, refused) = block(async move {
            let connection = connect(&address).await.expect("connect to the bus");
            let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
            set_enabled(&proxy, true).await;

            push(&proxy, "the first copy", 1_000).await;
            let refused = push(&proxy, "password=hunter2", 1_500).await;
            push(&proxy, "the second copy", 2_000).await;
            (list(&proxy).await, refused)
        });

        (listed, refused, harness.terminate().success())
    };

    assert_eq!(
        titles(&listed),
        vec!["the second copy".to_string(), "the first copy".to_string()],
        "the copies are recorded newest first"
    );
    assert!(!refused, "a copy that looks like a secret is refused");
    assert!(
        clean_exit,
        "the daemon exits on SIGTERM: the history is written as it changes, not at the end"
    );

    // ---- what is on the disk is the history, but not readable ----
    let on_disk = scratch.files();
    let history = on_disk
        .iter()
        .find(|(path, _)| path.ends_with("lumen/clipboard.bin"))
        .unwrap_or_else(|| panic!("the history file must exist, found {:?}", scratch.names()));
    for needle in [
        "the first copy",
        "the second copy",
        "hunter2",
        "timestamp_ms",
        "entries",
    ] {
        assert!(
            !contains(&history.1, needle),
            "`{needle}` is on disk in the clear"
        );
    }
    for (path, bytes) in &on_disk {
        assert!(
            !contains(bytes, "the first copy") && !contains(bytes, "hunter2"),
            "{path} holds clipboard text in the clear"
        );
    }

    // ---- a second daemon, same data directory and key file: the history is back ----
    let mut harness = scratch.spawn(&["--clipboard", "persist"]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (enabled, rows, matching) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let enabled: bool = proxy
            .get_property("ClipboardEnabled")
            .await
            .expect("ClipboardEnabled");
        let rows = list(&proxy).await;
        let matching = query(&proxy, "second", &["clipboard"]).await;
        (enabled, rows, matching)
    });

    assert!(enabled, "history comes back on after a restart");
    assert_eq!(
        titles(&rows),
        vec!["the second copy".to_string(), "the first copy".to_string()],
        "the history survived the restart"
    );
    assert_eq!(
        titles(&matching),
        vec!["the second copy".to_string()],
        "and the restored entries are searchable"
    );

    harness.terminate();
}

#[test]
fn a_corrupt_key_file_degrades_to_memory_and_says_so() {
    let scratch = Scratch::new("corrupt-key");
    // Present, but not a key: the daemon must not treat it as one, and must not fall over.
    std::fs::write(scratch.key_file(), "not-a-key").expect("write a corrupt key file");

    let mut harness = scratch.spawn(&["--clipboard", "persist"]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;
        push(&proxy, "recorded in memory", 1_000).await;
        list(&proxy).await
    });
    assert_eq!(
        titles(&rows),
        vec!["recorded in memory".to_string()],
        "the daemon keeps serving, in memory"
    );
    assert!(
        !scratch
            .names()
            .iter()
            .any(|name| name.ends_with("clipboard.bin")),
        "a degraded store writes no history: {:?}",
        scratch.names()
    );

    assert!(harness.terminate().success());
    let stderr = harness.stderr();
    assert!(
        stderr.contains("in memory only"),
        "the reason is on stderr: {stderr}"
    );
    assert!(
        stderr.contains("64 hex characters"),
        "and it names the problem: {stderr}"
    );
}

#[test]
fn a_secret_looking_copy_is_refused_and_never_written() {
    let scratch = Scratch::new("secret");

    let mut harness = scratch.spawn(&["--clipboard", "persist"]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (refused, ordinary, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;

        let refused = push(&proxy, "-----BEGIN OPENSSH PRIVATE KEY-----", 1_000).await;
        let ordinary = push(&proxy, "the quick brown fox", 2_000).await;
        (refused, ordinary, list(&proxy).await)
    });

    assert!(!refused, "a private key is not history");
    assert!(ordinary, "ordinary text still is");
    assert_eq!(titles(&rows), vec!["the quick brown fox".to_string()]);

    assert!(harness.terminate().success());
    for (path, bytes) in scratch.files() {
        assert!(
            !contains(&bytes, "OPENSSH PRIVATE KEY"),
            "{path} holds the private key in the clear"
        );
    }
}

/// A throw-away `HOME` and XDG tree, with the key file the daemon reads its key from — so a test
/// never writes to the developer's data directory or their keyring.
struct Scratch {
    root: std::path::PathBuf,
    env: Vec<(String, String)>,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "lumen-clipboard-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch home");

        let home = root.to_str().expect("a utf-8 path").to_string();
        let env = vec![
            ("HOME".to_string(), home.clone()),
            ("XDG_DATA_HOME".to_string(), format!("{home}/data")),
            ("XDG_CONFIG_HOME".to_string(), format!("{home}/config")),
            ("XDG_STATE_HOME".to_string(), format!("{home}/state")),
            ("XDG_CACHE_HOME".to_string(), format!("{home}/cache")),
            ("XDG_RUNTIME_DIR".to_string(), format!("{home}/run")),
            (
                "LUMEN_SECRET_KEY_FILE".to_string(),
                format!("{home}/key"),
            ),
        ];
        Self { root, env }
    }

    /// Where the daemon reads (and creates) its encryption key.
    fn key_file(&self) -> std::path::PathBuf {
        self.root.join("key")
    }

    fn spawn(&self, extra: &[&str]) -> Harness {
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        Harness::spawn_with_env(extra, &env)
    }

    /// Every name under the scratch tree, relative to it.
    fn names(&self) -> Vec<String> {
        self.files().into_iter().map(|(path, _)| path).collect()
    }

    /// Every file under the scratch tree, with its bytes.
    fn files(&self) -> Vec<(String, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![self.root.clone()];

        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    found.push((
                        path.strip_prefix(&self.root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned(),
                        bytes,
                    ));
                }
            }
        }

        found.sort();
        found
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Whether `haystack` contains `needle` as bytes.
fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}
