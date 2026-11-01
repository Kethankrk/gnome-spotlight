//! Clipboard history at the D-Bus seam: the real daemon binary, a real session bus, a real client.
//!
//! History is off until the user turns it on. While it is off nothing is recorded, `ClipboardList`
//! is empty, and the results carry one row that says so instead of an error. Nothing reaches the
//! filesystem at this stage: persistence is ticket 11.

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
fn clipboard_entries_are_matched_by_the_scorer_and_carry_a_copy_action() {
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
    assert_eq!(actions.len(), 1, "a clipboard entry has one action");
    assert_eq!(text_of(&actions[0], "id"), "copy");
    assert_eq!(text_of(&actions[0], "handler"), "copy");

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
fn nothing_reaches_the_filesystem() {
    // The daemon is given a throw-away HOME and XDG tree; recording copies must leave them empty.
    // Persistence, and with it a data directory, is ticket 11.
    let root = std::env::temp_dir().join(format!("lumen-clipboard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch home");
    let home = root.to_str().expect("a utf-8 path").to_string();

    let harness = Harness::spawn_with_env(&[], &[
        ("HOME", &home),
        ("XDG_DATA_HOME", &format!("{home}/data")),
        ("XDG_CONFIG_HOME", &format!("{home}/config")),
        ("XDG_STATE_HOME", &format!("{home}/state")),
        ("XDG_CACHE_HOME", &format!("{home}/cache")),
        ("XDG_RUNTIME_DIR", &format!("{home}/run")),
    ]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        set_enabled(&proxy, true).await;
        push(&proxy, "a secret nobody should find on disk", 1_000).await;
        list(&proxy).await
    });
    assert_eq!(rows.len(), 1, "the entry is in memory, and listed");

    let left_behind: Vec<String> = walk(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        left_behind.is_empty(),
        "clipboard history must not touch the filesystem yet, found {left_behind:?}"
    );
}

/// Every path under `root`, relative to it.
fn walk(root: &std::path::Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            found.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                pending.push(path);
            }
        }
    }

    found.sort();
    found
}
