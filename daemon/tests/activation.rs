//! Activation routing at the D-Bus seam: the real daemon binary, a real session bus, a real
//! client, and a real program the daemon opens files with (contract §2, §19).
//!
//! The daemon performs the actions it owns — opening a file or a recent document with the desktop's
//! default application — and hands the rest back to the shell (`copy`, `reveal`, `open-url`), which
//! is the only half that owns the clipboard, the file manager and the browser. `LUMEN_OPEN_BIN`
//! points the opener at a script that records what it was handed, so every outcome can be asserted
//! without a desktop and without launching anything.
//!
//! Each test gives the daemon a throw-away `HOME`/`XDG_*` tree, so a run here never reads or writes
//! the developer's own documents and usage counters.

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use support::{block, connect, engine_proxy, Harness};

/// `a{sv}`: one result-schema row, or the `Activate` reply.
type Row = HashMap<String, zbus::OwnedVariant>;
/// `aa{sv}`: the results of one search.
type Rows = Vec<Row>;

/// A program that cannot be started: the opener's failure path.
const MISSING_OPENER: &str = "/nonexistent/lumen-default-application";

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

fn has(row: &Row, key: &str) -> bool {
    row.contains_key(key)
}

/// A throw-away directory tree, and the opener program the daemon is pointed at.
struct Fixture {
    root: PathBuf,
    program: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lumen-activation-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(root.join("home")).expect("a throw-away home");
        std::fs::create_dir_all(root.join("xdg")).expect("a throw-away data home");

        // `$0` is the script's own path, so the log needs no second environment variable.
        let program = root.join("record-opener.sh");
        std::fs::write(&program, "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$0.opened\"\n")
            .expect("write the recording opener");
        let mut permissions = std::fs::metadata(&program)
            .expect("the opener's metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&program, permissions).expect("make the opener executable");

        let log = root.join("record-opener.sh.opened");
        Self { root, program, log }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn data_home(&self) -> PathBuf {
        self.root.join("xdg")
    }

    /// Write a document under the throw-away home and return its path.
    fn document(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.home().join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent directory"))
            .expect("create the document's directory");
        std::fs::write(&path, contents).expect("write the document");
        path
    }

    /// Write the recent-documents cache naming one document.
    fn write_recent_cache(&self, document: &std::path::Path) {
        let cache = self.data_home().join("recently-used.xbel");
        std::fs::write(
            cache,
            format!(
                "<xbel version=\"1.0\"><bookmark href=\"file://{}\" \
                 modified=\"2001-01-01T00:00:00Z\"/></xbel>",
                document.display()
            ),
        )
        .expect("write the cache");
    }

    /// Every uri the opener has been handed so far.
    fn opened(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Wait for the opener to record `uri`. The daemon does not wait for the program it started,
    /// so the reply can beat the record of it — as it does on a real desktop.
    fn wait_for(&self, uri: &str) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let opened = self.opened();
            if opened.iter().any(|entry| entry == uri) || Instant::now() > deadline {
                return opened;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A daemon under the fixture's throw-away home, opening files with the fixture's recording
/// program.
fn daemon(fixture: &Fixture) -> Harness {
    daemon_with(fixture, &fixture.program.display().to_string())
}

/// The same daemon with `LUMEN_OPEN_BIN` naming `program` instead.
fn daemon_with(fixture: &Fixture, program: &str) -> Harness {
    let home = fixture.home().display().to_string();
    let data = fixture.data_home().display().to_string();
    Harness::spawn_with_env(
        &[],
        &[
            ("HOME", home.as_str()),
            ("XDG_DATA_HOME", data.as_str()),
            ("XDG_STATE_HOME", data.as_str()),
            ("XDG_CONFIG_HOME", data.as_str()),
            ("LUMEN_OPEN_BIN", program),
        ],
    )
}

/// One `Query`, returning the results of the single `ResultsChanged` that answers it.
async fn search(proxy: &zbus::Proxy<'_>, text: &str, enabled: &[&str]) -> Rows {
    let mut changed = proxy
        .receive_signal("ResultsChanged")
        .await
        .expect("ResultsChanged stream");
    let enabled: Vec<String> = enabled.iter().map(|id| (*id).to_string()).collect();
    let id: String = proxy
        .call(
            "Query",
            &(text, enabled, HashMap::<String, zbus::OwnedVariant>::new()),
        )
        .await
        .expect("Query");
    assert!(!id.is_empty());

    let signal = changed.next().await.expect("ResultsChanged signal");
    let (_id, results): (String, Rows) = signal.body().deserialize().expect("body");
    results
}

async fn activate(proxy: &zbus::Proxy<'_>, result_id: &str, action_id: &str) -> zbus::Result<Row> {
    proxy.call("Activate", &(result_id, action_id)).await
}

#[test]
fn opening_a_file_is_performed_by_the_daemon() {
    const PATH: &str = "/home/me/notes/report.md";
    let fixture = Fixture::new("file-open");
    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let outcome = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        activate(&proxy, &format!("file:{PATH}"), "open")
            .await
            .expect("a file row opens")
    });

    assert!(
        bool_of(&outcome, "handled"),
        "the daemon performs this one itself: it has the default-application launcher (story 19)"
    );
    assert!(
        !has(&outcome, "shell_action") && !has(&outcome, "args"),
        "a performed action leaves nothing for the shell"
    );
    assert!(bool_of(&outcome, "recorded"), "and it is still recorded");
    assert_eq!(
        u32::try_from(&member(&outcome, "usage")).expect("a count"),
        1
    );

    let opened = fixture.wait_for(PATH);
    assert_eq!(
        opened,
        vec![PATH.to_string()],
        "the default application was handed the file's path"
    );
}

#[test]
fn a_file_that_cannot_be_opened_is_an_error_reply_rather_than_a_silent_no_op() {
    let fixture = Fixture::new("file-open-fails");
    let harness = daemon_with(&fixture, MISSING_OPENER);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let outcome = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        activate(&proxy, "file:/home/me/notes/report.md", "open").await
    });

    let error = outcome.expect_err("the launcher cannot be started");
    assert!(
        error.to_string().contains("report.md"),
        "the failure names what could not be opened: {error}"
    );
}

#[test]
fn revealing_a_file_is_handed_to_the_shell() {
    let fixture = Fixture::new("file-reveal");
    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let outcome = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        activate(&proxy, "file:/home/me/notes/report.md", "reveal")
            .await
            .expect("a file row reveals")
    });

    assert!(
        !bool_of(&outcome, "handled"),
        "the file manager is the shell's, so the daemon does not open it (story 20)"
    );
    assert_eq!(text_of(&outcome, "shell_action"), "reveal");
    assert_eq!(
        strings_of(&outcome, "args"),
        vec!["/home/me/notes/report.md".to_string()]
    );
    assert!(
        fixture.opened().is_empty(),
        "and nothing was opened: revealing is not opening"
    );
}

#[test]
fn copying_a_files_path_is_handed_to_the_shell() {
    let fixture = Fixture::new("file-copy");
    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let outcome = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        activate(&proxy, "file:/home/me/notes/report.md", "copy")
            .await
            .expect("a file row copies")
    });

    assert!(
        !bool_of(&outcome, "handled"),
        "only the shell owns the Wayland clipboard (story 21)"
    );
    assert_eq!(text_of(&outcome, "shell_action"), "copy");
    assert_eq!(
        strings_of(&outcome, "args"),
        vec!["/home/me/notes/report.md".to_string()],
        "the full path is what the shell puts on the clipboard"
    );
}

#[test]
fn opening_a_recent_document_is_performed_by_the_daemon() {
    let fixture = Fixture::new("recent-open");
    let report = fixture.document("notes/report.md", b"quarterly report");
    fixture.write_recent_cache(&report);

    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();
    let path = report.display().to_string();

    let (rows, outcome) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let rows = search(&proxy, "report", &["recent"]).await;
        let id = text_of(&rows[0], "id");
        let outcome = activate(&proxy, &id, "open").await.expect("a recent row opens");
        (rows, outcome)
    });

    assert_eq!(text_of(&rows[0], "id"), format!("recent:{path}"));
    assert!(
        bool_of(&outcome, "handled"),
        "a recent document opens like any other file (story 33)"
    );

    let opened = fixture.wait_for(&path);
    assert_eq!(
        opened,
        vec![path.clone()],
        "the document itself was opened, not its folder"
    );
}

#[test]
fn copying_a_calculation_is_handed_to_the_shell_at_its_value() {
    let fixture = Fixture::new("calc-copy");
    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (rows, outcome) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let rows = search(&proxy, "2 + 2", &["calculator"]).await;
        let id = text_of(&rows[0], "id");
        let outcome = activate(&proxy, &id, "copy").await.expect("a calculation copies");
        (rows, outcome)
    });

    assert_eq!(text_of(&rows[0], "id"), "calc:2 + 2");
    assert!(!bool_of(&outcome, "handled"), "the clipboard is the shell's");
    assert_eq!(text_of(&outcome, "shell_action"), "copy");
    assert_eq!(
        strings_of(&outcome, "args"),
        vec!["4".to_string()],
        "Enter on a calculated value copies the value (story 26)"
    );
}

#[test]
fn copying_a_clipboard_entry_is_handed_to_the_shell_at_its_text() {
    const COPIED: &str = "https://example.com/pricing";
    let fixture = Fixture::new("clip-copy");
    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (rows, outcome) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");

        let _: () = proxy
            .call("ClipboardSetEnabled", &(true,))
            .await
            .expect("ClipboardSetEnabled");
        let pushed: bool = proxy
            .call("ClipboardPush", &(COPIED, 1_700_000_000_000u64))
            .await
            .expect("ClipboardPush");
        assert!(pushed, "the copy is recorded while history is on");

        let rows = search(&proxy, "example", &["clipboard"]).await;
        let id = text_of(&rows[0], "id");
        let outcome = activate(&proxy, &id, "copy").await.expect("an entry copies");
        (rows, outcome)
    });

    assert!(text_of(&rows[0], "id").starts_with("clip:"));
    assert!(
        !bool_of(&outcome, "handled"),
        "only the shell owns the Wayland clipboard (story 46)"
    );
    assert_eq!(text_of(&outcome, "shell_action"), "copy");
    assert_eq!(
        strings_of(&outcome, "args"),
        vec![COPIED.to_string()],
        "the entry's text is what the shell puts back on the clipboard"
    );
}

#[test]
fn an_action_with_no_handler_reports_nothing_performed_and_a_foreign_id_is_refused() {
    let fixture = Fixture::new("unroutable");
    let harness = daemon(&fixture);
    let address = harness.address().to_string();
    let name = harness.name().to_string();

    let (no_handler, foreign, application) = block(async move {
        let connection = connect(&address).await.expect("connect to the bus");
        let proxy = engine_proxy(&connection, &name)
            .await
            .expect("engine proxy");
        let no_handler = activate(&proxy, "calc:2 + 2", "open")
            .await
            .expect("the row exists, its action has no handler");
        let foreign = activate(&proxy, "file:", "open").await;
        let application = activate(&proxy, "app:org.gnome.Nautilus.desktop", "open").await;
        (no_handler, foreign, application)
    });

    assert!(
        !bool_of(&no_handler, "handled") && !has(&no_handler, "shell_action"),
        "the calculator offers `copy` only, and neither half runs `open` on it"
    );
    assert!(bool_of(&no_handler, "recorded"), "the activation is still counted");

    assert!(
        foreign.is_err(),
        "an id with no result behind it is unknown, never opened"
    );
    let application = application.expect("an application row still answers");
    assert!(
        !bool_of(&application, "handled") && !has(&application, "shell_action"),
        "applications are the shell's own and are never routed by the engine"
    );
}
