//! Recent documents at the D-Bus seam.
//!
//! The real daemon, a real session bus, a client that only speaks the contract. The daemon is
//! pointed at a throw-away `HOME`/`XDG_DATA_HOME` holding a seeded `recently-used.xbel` and the
//! documents it names, so the test says exactly what the desktop would have written — and the
//! documents are real files, because a recent document that is no longer on disk is not a
//! document at all.

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, Harness};
use zbus::{OwnedVariant, Proxy};

/// `a{sv}`, the shape of `Query`'s `options` argument.
type Options = HashMap<String, OwnedVariant>;
/// `a{sv}`, one result row.
type Row = HashMap<String, OwnedVariant>;

/// A throw-away home directory, with its documents and its recent-documents cache.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lumen-recent-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("home")).expect("create the throw-away home");
        std::fs::create_dir_all(root.join("xdg")).expect("create the throw-away data home");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// `$XDG_DATA_HOME`, where the cache lives when it is set.
    fn data_home(&self) -> PathBuf {
        self.root.join("xdg")
    }

    /// The cache in `$HOME/.local/share`, where it lives when `XDG_DATA_HOME` is unset.
    fn home_cache(&self) -> PathBuf {
        self.home().join(".local/share/recently-used.xbel")
    }

    /// Write a document under the throw-away home and return its path.
    fn document(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.home().join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent directory"))
            .expect("create the document's directory");
        std::fs::write(&path, contents).expect("write the document");
        path
    }

    fn write_cache(&self, path: &Path, bookmarks: &[(&str, &str)]) {
        std::fs::create_dir_all(path.parent().expect("a parent directory"))
            .expect("create the cache's directory");
        std::fs::write(path, xbel(bookmarks)).expect("write the cache");
    }

    /// The document's `file://` URI, as the desktop writes it.
    fn uri(path: &Path) -> String {
        format!("file://{}", path.display())
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The cache file: one `<bookmark>` per `(href, modified)`. The recorded time is deliberately long
/// past in most of these tests, so the §5 recency bonus is zero and a result's score is the base
/// weight plus match quality — an assertion that does not depend on the clock.
fn xbel(bookmarks: &[(&str, &str)]) -> String {
    const OLD: &str = "2001-01-01T00:00:00Z";
    let body: Vec<String> = bookmarks
        .iter()
        .map(|(href, modified)| {
            let modified = if modified.is_empty() { OLD } else { modified };
            format!("<bookmark href=\"{href}\" modified=\"{modified}\"/>")
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xbel version=\"1.0\">{}</xbel>\n",
        body.join("")
    )
}

/// A daemon whose `$HOME` and `$XDG_DATA_HOME` are the sandbox's.
fn daemon_under(sandbox: &Sandbox) -> Harness {
    let home = sandbox.home().display().to_string();
    let data = sandbox.data_home().display().to_string();
    Harness::spawn_with_env(
        &[],
        &[("HOME", home.as_str()), ("XDG_DATA_HOME", data.as_str())],
    )
}

/// Run a search and return the rows the daemon pushed. `SearchFinished` must arrive too: a
/// search that never finishes is a failure even when rows came back.
async fn search(proxy: &Proxy<'_>, text: &str, enabled: &[&str]) -> Vec<Row> {
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
        .call("Query", &(text, enabled, Options::new()))
        .await
        .expect("Query must be accepted");

    let signal = changed.next().await.expect("a ResultsChanged signal");
    let (changed_id, results): (String, Vec<Row>) =
        signal.body().deserialize().expect("ResultsChanged body");
    let signal = finished.next().await.expect("a SearchFinished signal");
    let finished_id: String = signal.body().deserialize().expect("SearchFinished body");

    assert_eq!(changed_id, search_id, "the rows belong to this search");
    assert_eq!(
        finished_id, search_id,
        "exactly one SearchFinished per search"
    );
    results
}

fn text_of(row: &Row, key: &str) -> String {
    String::try_from(row[key].try_clone().expect("cloneable")).expect("a string")
}

fn number_of(row: &Row, key: &str) -> f64 {
    f64::try_from(row[key].try_clone().expect("cloneable")).expect("a number")
}

fn preview(row: &Row) -> HashMap<String, OwnedVariant> {
    HashMap::try_from(row["preview"].try_clone().expect("cloneable preview"))
        .expect("preview is an a{sv}")
}

fn preview_text(row: &Row, key: &str) -> String {
    String::try_from(preview(row)[key].try_clone().expect("cloneable")).expect("a string")
}

fn actions_of(row: &Row) -> Vec<(String, String, String)> {
    Vec::<Row>::try_from(row["actions"].try_clone().expect("cloneable actions"))
        .expect("actions is an aa{sv}")
        .iter()
        .map(|action| {
            (
                text_of(action, "id"),
                text_of(action, "label"),
                text_of(action, "handler"),
            )
        })
        .collect()
}

fn ids(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|row| text_of(row, "id")).collect()
}

#[test]
fn recent_documents_come_back_as_results_that_open() {
    let sandbox = Sandbox::new("results");
    let report = sandbox.document("notes/report.md", b"quarterly report");
    let daily = sandbox.document("notes/daily-report.md", b"daily");
    let inside = sandbox.document("notes/myreport.md", b"inside a word");
    let budget = sandbox.document("notes/budget.md", b"unrelated");

    // Noise the file rules exclude, a document that is gone, and a URI that is not a file: none
    // of them may reach the user (user story 34).
    let dot_cache = sandbox.document(".cache/report.md", b"thumbnail cache");
    let git = sandbox.document("repo/.git/report.txt", b"vcs metadata");
    let modules = sandbox.document("app/node_modules/report/index.js", b"dependency");
    let gone = sandbox.home().join("notes/gone.md");

    sandbox.write_cache(
        &sandbox.data_home().join("recently-used.xbel"),
        &[
            (&Sandbox::uri(&report), ""),
            (&Sandbox::uri(&daily), ""),
            (&Sandbox::uri(&inside), ""),
            (&Sandbox::uri(&budget), ""),
            (&Sandbox::uri(&dot_cache), ""),
            (&Sandbox::uri(&git), ""),
            (&Sandbox::uri(&modules), ""),
            (&Sandbox::uri(&gone), ""),
            ("https://example.com/notes", "2026-11-01T00:00:00Z"),
        ],
    );

    let harness = daemon_under(&sandbox);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (capabilities, rows) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let capabilities: Vec<String> = proxy
            .get_property("Capabilities")
            .await
            .expect("Capabilities");
        let rows = search(&proxy, "report", &["recent"]).await;
        (capabilities, rows)
    });

    assert!(
        capabilities.contains(&"recent".to_string()),
        "recent documents are advertised: {capabilities:?}"
    );

    // Best match first: an exact prefix beats a word start beats a substring — and the excluded,
    // missing and non-file places are not in the list at all.
    assert_eq!(
        ids(&rows),
        vec![
            format!("recent:{}", report.display()),
            format!("recent:{}", daily.display()),
            format!("recent:{}", inside.display()),
        ]
    );
    for id in ids(&rows) {
        for noise in [".cache", ".git", "node_modules", "gone.md", "example.com"] {
            assert!(!id.contains(noise), "{id} must not reach the user");
        }
    }

    let row = &rows[0];
    assert_eq!(text_of(row, "provider"), "recent");
    assert_eq!(text_of(row, "title"), "report.md");
    assert_eq!(
        text_of(row, "subtitle"),
        "~/notes",
        "the containing folder, with home written as ~"
    );
    assert_eq!(text_of(row, "icon"), "text-x-generic");
    assert_eq!(
        actions_of(row),
        vec![("open".to_string(), "Open".to_string(), "open".to_string())],
        "Enter opens the document"
    );
    assert!(
        (number_of(row, "score") - 0.98).abs() < 1e-9,
        "the recent base weight 0.18 plus prefix quality 0.8, got {}",
        number_of(row, "score")
    );

    assert_eq!(preview_text(row, "kind"), "recent");
    assert_eq!(
        preview_text(row, "path"),
        report.display().to_string(),
        "the preview carries the local path, not the URI"
    );
    assert_eq!(preview_text(row, "folder"), "~/notes");
    let payload = preview(row);
    assert_eq!(payload["size"].value_signature().to_string(), "t");
    assert_eq!(payload["mtime"].value_signature().to_string(), "t");
    assert_eq!(
        u64::try_from(payload["size"].try_clone().expect("cloneable")).expect("a size"),
        b"quarterly report".len() as u64
    );
    assert!(
        u64::try_from(payload["mtime"].try_clone().expect("cloneable")).expect("an mtime") > 0,
        "the file's own modification time comes from the filesystem"
    );
}

#[test]
fn the_cache_is_read_when_the_query_is_made() {
    let sandbox = Sandbox::new("live");
    let report = sandbox.document("notes/report.md", b"the first document");
    // The second document exists from the start; what changes is that the desktop records it as
    // opened while the daemon is already running.
    let later = sandbox.document("notes/later-report.md", b"opened later");
    let cache = sandbox.data_home().join("recently-used.xbel");
    let report_uri = Sandbox::uri(&report);
    let later_uri = Sandbox::uri(&later);
    sandbox.write_cache(&cache, &[(&report_uri, "")]);
    let later_id = format!("recent:{}", later.display());

    let harness = daemon_under(&sandbox);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (before, after) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");

        let before = search(&proxy, "report", &["recent"]).await;

        sandbox.write_cache(
            &cache,
            &[(&report_uri, ""), (&later_uri, "2026-10-30T08:00:00Z")],
        );

        let after = search(&proxy, "report", &["recent"]).await;
        (before, after)
    });

    assert_eq!(
        before.len(),
        1,
        "only the document the cache named at first"
    );
    assert_eq!(
        after.len(),
        2,
        "a document opened since startup is in the next search: the cache is read at query time"
    );
    assert!(
        ids(&after).contains(&later_id),
        "{:?} does not contain {later_id}",
        ids(&after)
    );
}

#[test]
fn without_xdg_data_home_the_cache_comes_from_the_home_directory() {
    let sandbox = Sandbox::new("fallback");
    let report = sandbox.document("notes/report.md", b"a document");
    sandbox.write_cache(&sandbox.home_cache(), &[(&Sandbox::uri(&report), "")]);

    // An empty XDG_DATA_HOME is not a directory: the cache is `$HOME/.local/share`, per the XDG
    // base directory specification.
    let home = sandbox.home().display().to_string();
    let harness = Harness::spawn_with_env(&[], &[("HOME", home.as_str()), ("XDG_DATA_HOME", "")]);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let rows = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        search(&proxy, "report", &["recent"]).await
    });

    assert_eq!(ids(&rows), vec![format!("recent:{}", report.display())]);
}

#[test]
fn a_user_with_no_cache_gets_no_recent_documents_rather_than_an_error() {
    let sandbox = Sandbox::new("empty");
    let harness = daemon_under(&sandbox);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (capabilities, rows, empty_query) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let capabilities: Vec<String> = proxy
            .get_property("Capabilities")
            .await
            .expect("Capabilities");
        let rows = search(&proxy, "report", &["recent"]).await;
        let empty_query = search(&proxy, "", &["recent"]).await;
        (capabilities, rows, empty_query)
    });

    assert!(
        capabilities.contains(&"recent".to_string()),
        "the provider exists; this user simply has no documents yet: {capabilities:?}"
    );
    assert!(rows.is_empty(), "nothing in the cache, nothing in the list");
    assert!(
        empty_query.is_empty(),
        "an empty query is not a search for every recent document"
    );
}
