//! The web-search fallback at the D-Bus seam: the real daemon binary, a real session bus, a real
//! client.
//!
//! The row is offered only when the query matched nothing strongly, it carries the url as an
//! `open-url` shell action rather than opening anything itself, and the engine behind the url is
//! the shared schema's `search-engine` key — resolved through `GSETTINGS_SCHEMA_DIR`, with the
//! contract default when GSettings cannot answer.

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, injected, injected_options, Harness};

/// `a{sv}`: the shape of `Query`'s `options`, of a result row and of `Activate`'s reply.
type Row = HashMap<String, zbus::OwnedVariant>;
/// `aa{sv}`: the results of one search.
type Rows = Vec<Row>;

const QUERY: &str = "how tall is everest";
const DEFAULT_URL: &str = "https://duckduckgo.com/?q=how%20tall%20is%20everest";
const DEFAULT_ID: &str = "web:2da932bf0e08b58f3658675998b6c67bca16556a";
const ENGINE: &str = "https://searx.example/search?q=%s";
const ENGINE_URL: &str = "https://searx.example/search?q=how%20tall%20is%20everest";
const ENGINE_ID: &str = "web:05101ca16e0077565b38ed51b990f2657c876741";

fn member(row: &Row, key: &str) -> zbus::Variant<'static> {
    zbus::Variant::from(row[key].try_clone().expect("cloneable"))
}

fn text_of(row: &Row, key: &str) -> String {
    String::try_from(&member(row, key)).expect("a string")
}

fn bool_of(row: &Row, key: &str) -> bool {
    bool::try_from(&member(row, key)).expect("a boolean")
}

fn strings_of(row: &Row, key: &str) -> Vec<String> {
    Vec::try_from(member(row, key)).expect("an array of strings")
}

fn providers(rows: &Rows) -> Vec<String> {
    rows.iter().map(|row| text_of(row, "provider")).collect()
}

fn preview_of(row: &Row) -> Row {
    HashMap::try_from(member(row, "preview")).expect("the preview must be an a{sv}")
}

fn actions_of(row: &Row) -> Vec<Row> {
    Vec::try_from(member(row, "actions")).expect("actions must be aa{sv}")
}

/// A throw-away directory, unique to this test process and one name.
fn temp_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lumen-websearch-it-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("create a temp dir");
    dir
}

/// A daemon with no schema in reach: `gsettings` cannot answer, so the row has to fall back to
/// the contract default (contract §9). Deterministic on every machine.
fn harness_without_a_schema() -> Harness {
    let dir = temp_dir("empty-schemas").to_string_lossy().into_owned();
    let env = [("GSETTINGS_SCHEMA_DIR", dir.as_str())];
    Harness::spawn_with_env(&[], &env)
}

/// Compile the repository's schema with `search-engine` defaulting to `engine` into a throw-away
/// directory. `None` when `glib-compile-schemas` is not installed, so the test can skip.
fn compiled_schema_dir(engine: &str) -> Option<PathBuf> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../extension/schemas/org.gnome.shell.extensions.lumen.gschema.xml");
    let xml = std::fs::read_to_string(source).ok()?;
    let xml = xml.replace(
        "<default>'https://duckduckgo.com/?q=%s'</default>",
        &format!("<default>'{engine}'</default>"),
    );

    let dir = temp_dir("schema");
    std::fs::write(dir.join("org.gnome.shell.extensions.lumen.gschema.xml"), xml).ok()?;
    let compiled = std::process::Command::new("glib-compile-schemas")
        .arg(&dir)
        .status()
        .ok()?;
    compiled.success().then_some(dir)
}

/// One `Query`, returning the results of the single `ResultsChanged` that answers it.
async fn search(proxy: &zbus::Proxy<'_>, text: &str, enabled: Vec<&str>) -> Rows {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let _: String = proxy
        .call("Query", &(text, enabled, HashMap::<String, zbus::OwnedVariant>::new()))
        .await
        .expect("Query");
    let signal = changed.next().await.expect("ResultsChanged signal");
    let (_id, results): (String, Rows) = signal.body().deserialize().expect("ResultsChanged body");
    results
}

/// One `Query` with caller-computed candidates, as the extension issues it.
async fn search_with(proxy: &zbus::Proxy<'_>, text: &str, enabled: Vec<&str>, options: Row) -> Rows {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let _: String = proxy
        .call("Query", &(text, enabled, options))
        .await
        .expect("Query");
    let signal = changed.next().await.expect("ResultsChanged signal");
    let (_id, results): (String, Rows) = signal.body().deserialize().expect("ResultsChanged body");
    results
}

async fn activate(proxy: &zbus::Proxy<'_>, result_id: &str, action_id: &str) -> zbus::Result<Row> {
    proxy.call("Activate", &(result_id, action_id)).await
}

#[test]
fn a_query_that_matches_nothing_is_offered_the_web_search_row() {
    let harness = harness_without_a_schema();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let results = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        search(
            &proxy,
            QUERY,
            vec!["applications", "files", "calculator", "websearch"],
        )
        .await
    });

    assert_eq!(results.len(), 1, "one row, and never an error row");
    let row = &results[0];

    assert_eq!(text_of(row, "provider"), "websearch");
    assert_eq!(text_of(row, "id"), DEFAULT_ID, "the id hashes the url");
    assert_eq!(text_of(row, "title"), format!("Search the web for {QUERY}"));
    assert_eq!(text_of(row, "subtitle"), "");
    assert_eq!(
        f64::try_from(&member(row, "score")).expect("a number"),
        0.05,
        "the last resort is scored by its base weight alone"
    );
    assert!(
        preview_of(row).is_empty(),
        "contract §6: an empty preview, and the pane degrades gracefully"
    );

    let actions = actions_of(row);
    assert_eq!(actions.len(), 1, "the row offers one action");
    assert_eq!(text_of(&actions[0], "id"), "open");
    assert_eq!(text_of(&actions[0], "label"), "Search");
    assert_eq!(
        text_of(&actions[0], "handler"),
        "shell",
        "the daemon cannot open a browser: the shell must"
    );
}

#[test]
fn a_strong_match_leaves_no_web_search_row_and_a_weak_one_keeps_it() {
    let harness = harness_without_a_schema();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (strong, weak) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");

        let strong = search_with(
            &proxy,
            "files",
            vec!["applications", "websearch"],
            injected_options(vec![injected(
                "app:org.gnome.Nautilus.desktop",
                "applications",
                "Files",
            )]),
        )
        .await;

        let weak = search_with(
            &proxy,
            "term",
            vec!["files", "websearch"],
            injected_options(vec![injected(
                "file:/home/me/xterm-settings.txt",
                "files",
                "xterm-settings.txt",
            )]),
        )
        .await;

        (strong, weak)
    });

    assert_eq!(
        providers(&strong),
        vec!["applications".to_string()],
        "an exact application match means the user is not searching the web"
    );
    assert_eq!(
        providers(&weak),
        vec!["files".to_string(), "websearch".to_string()],
        "a substring match is weak: the row is offered, and sorts last"
    );
    assert_eq!(text_of(&weak[1], "title"), "Search the web for term");
}

#[test]
fn an_empty_query_is_offered_nothing() {
    let harness = harness_without_a_schema();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (empty, blank) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let empty = search(&proxy, "", vec!["websearch"]).await;
        let blank = search(&proxy, "   ", vec!["websearch"]).await;
        (empty, blank)
    });

    assert!(empty.is_empty(), "there is nothing to search the web for");
    assert!(blank.is_empty());
}

#[test]
fn activating_the_row_hands_the_url_to_the_shell_instead_of_opening_it() {
    let harness = harness_without_a_schema();
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (results, outcome, unknown, daemon_owned) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");

        let results = search(&proxy, QUERY, vec!["websearch"]).await;
        let id = text_of(&results[0], "id");

        let outcome = activate(&proxy, &id, "open").await.expect("Activate");
        let unknown = activate(&proxy, "web:0000000000000000000000000000000000000000", "open").await;
        let daemon_owned = activate(&proxy, "file:/home/me/report.pdf", "open").await;

        (results, outcome, unknown, daemon_owned)
    });

    assert_eq!(text_of(&results[0], "id"), DEFAULT_ID);
    assert_eq!(
        bool_of(&outcome, "handled"),
        false,
        "the daemon must not do this itself: it has no browser and makes no network request"
    );
    assert_eq!(text_of(&outcome, "shell_action"), "open-url");
    assert_eq!(
        strings_of(&outcome, "args"),
        vec![DEFAULT_URL.to_string()],
        "the shell gets the url the configured engine built"
    );

    assert!(
        unknown.is_err(),
        "a row this session never offered has no url to route"
    );
    assert!(
        daemon_owned.is_err(),
        "a daemon-owned row is not shell-routed: its handler lands with its ticket"
    );
}

#[test]
fn the_search_engine_setting_is_what_builds_the_url() {
    let Some(schema_dir) = compiled_schema_dir(ENGINE) else {
        eprintln!("skipped: glib-compile-schemas is not installed");
        return;
    };

    let dir = schema_dir.to_string_lossy().into_owned();
    let env = [("GSETTINGS_SCHEMA_DIR", dir.as_str())];
    let harness = Harness::spawn_with_env(&[], &env);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (results, outcome) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let results = search(&proxy, QUERY, vec!["websearch"]).await;
        let id = text_of(&results[0], "id");
        let outcome = activate(&proxy, &id, "open").await.expect("Activate");
        (results, outcome)
    });

    assert_eq!(
        text_of(&results[0], "id"),
        ENGINE_ID,
        "the row hashes the url the schema's engine builds"
    );
    assert_eq!(
        strings_of(&outcome, "args"),
        vec![ENGINE_URL.to_string()],
        "and that is the url the shell opens"
    );
}
