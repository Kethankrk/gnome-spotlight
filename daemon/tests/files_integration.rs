//! File and folder search at the D-Bus seam.
//!
//! The real daemon, a real session bus, a client that only speaks the contract: queries go in,
//! `ResultsChanged` rows come out. The index is the seeded fake, so the tests say exactly what
//! the index knows — and one `#[ignore]`d test runs the same path against the machine's real
//! LocalSearch endpoint.

mod support;

use std::collections::HashMap;
use std::path::PathBuf;

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, Harness};
use zbus::{OwnedVariant, Proxy};

/// `a{sv}`, the shape of `Query`'s `options` argument.
type Options = HashMap<String, OwnedVariant>;
/// `a{sv}`, one result row.
type Row = HashMap<String, OwnedVariant>;

/// A seed file written for one test and removed with it.
struct Seed {
    path: PathBuf,
}

impl Seed {
    fn new(name: &str, json: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "lumen-files-{}-{name}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).expect("write the seed file");
        Self { path }
    }

    /// The `--indexer` value that points the daemon at this seed.
    fn spec(&self) -> String {
        format!("fake:{}", self.path.display())
    }
}

impl Drop for Seed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A seed of files that all match `report`.
fn report_seed(count: usize) -> Seed {
    let files: Vec<String> = (0..count)
        .map(|n| {
            format!(
                r#"{{"path": "/home/me/notes/report-{n:02}.md", "size": {size}, "mtime_ms": {mtime}}}"#,
                size = 1024 + n,
                mtime = 1_700_000_000_000_u64 + n as u64
            )
        })
        .collect();
    Seed::new(
        &format!("reports-{count}"),
        &format!(r#"{{"files": [{}]}}"#, files.join(",")),
    )
}

/// A daemon whose index is the seed, with a pinned HOME so subtitles are predictable.
fn daemon_with(seed: &Seed) -> Harness {
    let spec = seed.spec();
    Harness::spawn_with_env(&["--indexer", spec.as_str()], &[("HOME", "/home/me")])
}

/// Run a search and return the rows the daemon pushed. `SearchFinished` must arrive too: a
/// search that never finishes is a failure even when rows came back.
async fn search(proxy: &Proxy<'_>, text: &str, enabled: &[&str], options: Options) -> Vec<Row> {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let mut finished = proxy
        .receive_signal("SearchFinished")
        .await
        .expect("SearchFinished stream");

    let enabled: Vec<String> = enabled.iter().map(|id| (*id).to_string()).collect();
    let search_id: String = proxy
        .call("Query", &(text, enabled, options))
        .await
        .expect("Query must be accepted");

    let signal = changed.next().await.expect("a ResultsChanged signal");
    let (changed_id, results): (String, Vec<Row>) =
        signal.body().deserialize().expect("ResultsChanged body");
    let signal = finished.next().await.expect("a SearchFinished signal");
    let finished_id: String = signal.body().deserialize().expect("SearchFinished body");

    assert_eq!(changed_id, search_id, "the rows belong to this search");
    assert_eq!(finished_id, search_id, "exactly one SearchFinished per search");
    results
}

/// `options` with one `u` entry, as the extension sends it.
fn options_with(entries: &[(&str, u64)]) -> Options {
    let mut options = Options::new();
    for (key, value) in entries {
        options.insert(
            key.to_string(),
            OwnedVariant::try_from(zbus::Variant::from(*value)).expect("an owned u"),
        );
    }
    options
}

fn text_of(row: &Row, key: &str) -> String {
    String::try_from(row[key].try_clone().expect("cloneable")).expect("a string")
}

fn field(row: &Row, map: &str, key: &str) -> OwnedVariant {
    let map = HashMap::<String, OwnedVariant>::try_from(
        row[map].try_clone().expect("cloneable map"),
    )
    .expect("a map");
    map[key].try_clone().expect("cloneable field")
}

fn preview_text(row: &Row, key: &str) -> String {
    String::try_from(field(row, "preview", key)).expect("a string")
}

fn id_of(row: &Row) -> String {
    text_of(row, "id")
}

#[test]
fn a_file_query_returns_hits_with_actions_and_a_preview() {
    // HOME is pinned so the subtitle is part of the assertion rather than of the machine.
    let seed = Seed::new(
        "one-report",
        r#"{"files": [
            {"path": "/home/me/notes/report.md", "size": 2048, "mtime_ms": 1700000000000},
            {"path": "/home/me/notes/unrelated.md", "size": 10}
        ]}"#,
    );
    let harness = daemon_with(&seed);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        search(&proxy, "report", &["files"], Options::new()).await
    });

    assert_eq!(rows.len(), 1, "only the matching file comes back");
    let row = &rows[0];

    assert_eq!(text_of(row, "provider"), "files");
    assert_eq!(id_of(row), "file:/home/me/notes/report.md");
    assert_eq!(text_of(row, "title"), "report.md");
    assert_eq!(text_of(row, "subtitle"), "~/notes");
    assert_eq!(text_of(row, "icon"), "text-x-generic");

    let actions = row["actions"]
        .try_clone()
        .ok()
        .and_then(|v| Vec::<Row>::try_from(v).ok())
        .expect("actions is an aa{sv}");
    let described: Vec<(String, String, String)> = actions
        .iter()
        .map(|action| {
            (
                text_of(action, "id"),
                text_of(action, "label"),
                text_of(action, "handler"),
            )
        })
        .collect();
    assert_eq!(
        described,
        vec![
            ("open".to_string(), "Open".to_string(), "open".to_string()),
            (
                "reveal".to_string(),
                "Show in Files".to_string(),
                "reveal".to_string()
            ),
            (
                "copy".to_string(),
                "Copy Path".to_string(),
                "copy".to_string()
            ),
        ],
        "Enter opens, the modifier reveals, the other copies the path"
    );

    assert_eq!(preview_text(row, "kind"), "file");
    assert_eq!(preview_text(row, "path"), "/home/me/notes/report.md");
    assert_eq!(preview_text(row, "folder"), "~/notes");
    assert_eq!(field(row, "preview", "size").value_signature().to_string(), "t");
    assert_eq!(field(row, "preview", "mtime").value_signature().to_string(), "t");
    assert_eq!(
        u64::try_from(field(row, "preview", "size")).expect("a size"),
        2048
    );
    assert_eq!(
        u64::try_from(field(row, "preview", "mtime")).expect("an mtime"),
        1_700_000_000_000
    );
}

#[test]
fn a_folder_hit_says_it_is_a_folder() {
    let seed = Seed::new(
        "folder",
        r#"{"files": [{"path": "/home/me/Projects/lumen", "folder": true}]}"#,
    );
    let harness = daemon_with(&seed);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        search(&proxy, "lumen", &["files"], Options::new()).await
    });

    assert_eq!(rows.len(), 1);
    assert_eq!(text_of(&rows[0], "icon"), "folder");
    assert_eq!(preview_text(&rows[0], "kind"), "folder");
    assert_eq!(text_of(&rows[0], "title"), "lumen");
}

#[test]
fn excluded_paths_never_reach_the_results() {
    let seed = Seed::new(
        "noise",
        r#"{"files": [
            {"path": "/home/me/notes/report.md"},
            {"path": "/home/me/.git/report-commits.txt"},
            {"path": "/home/me/.config/report.conf"},
            {"path": "/home/me/app/node_modules/report/index.js"},
            {"path": "/home/me/app/target/report.log"},
            {"path": "/home/me/repo/.svn/report"},
            {"path": "/home/me/repo/.hg/report"}
        ]}"#,
    );
    let harness = daemon_with(&seed);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        search(&proxy, "report", &["files"], Options::new()).await
    });

    let ids: Vec<String> = rows.iter().map(id_of).collect();
    assert_eq!(
        ids,
        vec!["file:/home/me/notes/report.md".to_string()],
        "dot-directories, VCS directories and build noise are excluded with no configuration"
    );
}

#[test]
fn file_results_are_capped_and_paged() {
    let seed = report_seed(12);
    let harness = daemon_with(&seed);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (first, second, huge) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");

        let first = search(&proxy, "report", &["files"], Options::new()).await;
        let second = search(&proxy, "report", &["files"], options_with(&[("offset", 8)])).await;
        let huge = search(&proxy, "report", &["files"], options_with(&[("limit", 1000)])).await;
        (first, second, huge)
    });

    assert_eq!(first.len(), 8, "a broad query cannot flood the list");
    assert_eq!(second.len(), 4, "the second page holds the rest");
    assert_eq!(huge.len(), 12, "a caller may ask for more, up to the provider's cap");

    let first_ids: Vec<String> = first.iter().map(id_of).collect();
    let second_ids: Vec<String> = second.iter().map(id_of).collect();
    assert_eq!(
        first_ids.first().map(String::as_str),
        Some("file:/home/me/notes/report-00.md")
    );
    assert_eq!(
        second_ids.first().map(String::as_str),
        Some("file:/home/me/notes/report-08.md"),
        "paging continues where the previous page stopped"
    );
    for id in &second_ids {
        assert!(!first_ids.contains(id), "{id} is on both pages");
    }
}

#[test]
fn the_files_capability_is_advertised_only_with_an_index() {
    let seed = report_seed(1);
    let spec = seed.spec();
    let harness = Harness::spawn_with_args(&["--indexer", spec.as_str()]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let capabilities: Vec<String> = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        proxy.get_property("Capabilities").await.expect("Capabilities")
    });

    assert!(capabilities.contains(&"files".to_string()), "{capabilities:?}");
}

#[test]
fn without_the_index_files_is_absent_and_applications_still_work() {
    // The library path is a documented testing seam; pointing it at nothing is exactly the
    // machine that has no TinySPARQL (contract §9).
    let harness = Harness::spawn_with_env(
        &["--indexer", "localsearch"],
        &[("LUMEN_TINYSPARQL_LIB", "/nonexistent/libtinysparql-3.0.so.0")],
    );
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (capabilities, files_rows, mixed_rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let capabilities: Vec<String> = proxy.get_property("Capabilities").await.expect("Capabilities");

        let files_rows = search(&proxy, "report", &["files"], Options::new()).await;

        let options = support::injected_options(vec![support::injected(
            "app:org.gnome.Nautilus.desktop",
            "applications",
            "Files",
        )]);
        let mixed = search(&proxy, "report", &["applications", "files"], options).await;
        (capabilities, files_rows, mixed)
    });

    assert!(
        !capabilities.contains(&"files".to_string()),
        "no index means no files capability: {capabilities:?}"
    );
    assert!(
        capabilities.contains(&"calculator".to_string()),
        "calculation is still advertised: {capabilities:?}"
    );
    assert!(files_rows.is_empty(), "the file provider answers nothing");
    assert_eq!(mixed_rows.len(), 1, "the applications result still comes through");
    assert_eq!(text_of(&mixed_rows[0], "provider"), "applications");
}

/// The same code path against the machine's real GNOME LocalSearch endpoint.
///
/// Run with `cargo test -- --ignored live_localsearch`, from a shell that has
/// `DBUS_SESSION_BUS_ADDRESS` and a running LocalSearch. It is not part of the default suite
/// because it depends on what the developer's machine has indexed.
#[test]
#[ignore = "needs a live LocalSearch endpoint on the session bus"]
fn live_localsearch_answers_a_file_query() {
    let harness = Harness::spawn_with_args(&["--indexer", "localsearch"]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (capabilities, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name).await.expect("engine proxy");
        let capabilities: Vec<String> = proxy.get_property("Capabilities").await.expect("Capabilities");
        let rows = search(&proxy, "readme", &["files"], Options::new()).await;
        (capabilities, rows)
    });

    assert!(
        capabilities.contains(&"files".to_string()),
        "LocalSearch should be available here: {capabilities:?}"
    );
    assert!(!rows.is_empty(), "the index should know some README files");
    for (at, row) in rows.iter().enumerate().take(5) {
        println!(
            "  {:>2}. {} — {} ({})",
            at + 1,
            text_of(row, "title"),
            text_of(row, "subtitle"),
            text_of(row, "icon")
        );
    }
    for row in &rows {
        assert_eq!(text_of(row, "provider"), "files");
        let id = id_of(row);
        assert!(id.starts_with("file:/"), "{id}");
        assert!(!id.contains("/."), "{id} is in a dot-directory");
    }
}
