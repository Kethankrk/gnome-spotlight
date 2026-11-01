//! The `org.gnome.Lumen.Engine` interface, and the search pipeline behind it.
//!
//! `Query` returns a search id straight away and hands the work to the search thread, which
//! emits `ResultsChanged` followed by exactly one `SearchFinished`. Nothing on the D-Bus dispatch
//! thread does provider work.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::{interface, Connection, OwnedVariant, Variant};

use crate::opener::Opener;
use crate::protocol::{self, ResultItem, Row, Rows};
use crate::providers::clipboard::entry_row;
use crate::providers::websearch::{self, Routes};
use crate::providers::QueryRequest;
use crate::registry::{Indexer, Registry};
use crate::store::clipboard::ClipboardStore;
use crate::store::usage::UsageStore;
use crate::store::{self, ClipboardMode};

/// Everything the daemon needs to serve its side of the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The well-known name to own. Owning it is the single-instance lock.
    pub dbus_name: String,
    /// Where the engine interface is exported.
    pub object_path: String,
    /// How to reach LocalSearch/TinySPARQL, if at all.
    pub indexer: Indexer,
    /// Whether clipboard history is written down, and where.
    pub clipboard: ClipboardMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dbus_name: protocol::BUS_NAME.to_string(),
            object_path: protocol::OBJECT_PATH.to_string(),
            indexer: Indexer::default(),
            clipboard: ClipboardMode::default(),
        }
    }
}

/// A daemon that owns its name and is answering on the bus. Dropping it releases the name.
pub struct Serving {
    pub connection: Connection,
    pub name: String,
}

struct SearchJob {
    number: u64,
    id: String,
    request: QueryRequest,
}

/// The exported engine object.
pub struct Engine {
    registry: Arc<Registry>,
    jobs: mpsc::Sender<SearchJob>,
    issued: Arc<AtomicU64>,
    /// The one clipboard history, shared with the clipboard provider inside the registry.
    clipboard: Arc<ClipboardStore>,
    /// The urls the web-search rows this daemon offered point at, so `Activate` can route their
    /// ids back to them (the id is `web:<sha1(url)>`, which cannot be reversed).
    routes: Routes,
    /// The program that opens a file with the desktop's default application (contract §19).
    opener: Opener,
    /// What the user has been activating, shared with the registry's scorer (ticket 13).
    usage: Arc<UsageStore>,
}

/// What `Activate` does with one (result, action) pair (contract §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routing {
    /// The daemon performed the action itself.
    Handled,
    /// The daemon opens this uri with the desktop's default application: `open` on a file or a
    /// recent document, which is the one action the daemon can perform without the shell.
    Open { uri: String },
    /// The shell performs it: `shell_action` names it and `args` are its arguments. The daemon
    /// does nothing with them — `open-url` is the browser's (contract §9: Lumen makes no network
    /// request of its own), and `copy`/`reveal` are the shell's because only the shell owns the
    /// Wayland clipboard and the file manager.
    Shell { action: String, args: Vec<String> },
}

/// What routing a result needs besides its id: the web-search url table (a `web:<sha1(url)>` id
/// cannot be reversed) and the clipboard history (a `clip:<digest>` id cannot be either).
pub struct Session<'a> {
    pub urls: &'a Routes,
    pub clipboard: &'a ClipboardStore,
}

/// Why an `Activate` cannot be routed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unroutable {
    /// An action on a result that has no handler: a `calc:` row offers `copy` and nothing else, an
    /// `app:`/`action:`/`setting:` row is the shell's own and never reaches the engine.
    NoHandler { provider: String, action: String },
    /// No row with that id in this session. The tables only hold what this daemon offered.
    UnknownResult(String),
}

impl std::fmt::Display for Unroutable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHandler { provider, action } => write!(
                formatter,
                "Lumen cannot perform `{action}` on a `{provider}` result"
            ),
            Self::UnknownResult(id) => write!(formatter, "Lumen does not know the result `{id}`"),
        }
    }
}

/// Route one action on one result (contract §2).
///
/// Daemon-owned actions either resolve to [`Routing::Open`] (the daemon opens the file) or to
/// [`Routing::Handled`]; a shell action is *not* performed here but named in the reply, and the
/// extension runs it. The decision itself is pure and does no I/O, so every outcome is testable
/// without opening anything.
pub fn route(result_id: &str, action_id: &str, session: &Session<'_>) -> Result<Routing, Unroutable> {
    let Some(provider) = protocol::provider_of_id(result_id) else {
        return Err(Unroutable::UnknownResult(result_id.to_string()));
    };

    let routing = match (provider, action_id) {
        (websearch::PROVIDER, websearch::OPEN_ACTION) => match session.urls.url_of(result_id) {
            Some(url) => Routing::Shell {
                action: protocol::OPEN_URL.to_string(),
                args: vec![url],
            },
            None => return Err(Unroutable::UnknownResult(result_id.to_string())),
        },
        // Opening a file or a recent document is the daemon's (it has the default-application
        // launcher); showing it in Files and copying its path are the shell's.
        ("files" | "recent", protocol::OPEN) => Routing::Open {
            uri: payload(result_id)?,
        },
        ("files", protocol::REVEAL) => shell(protocol::REVEAL, payload(result_id)?),
        ("files", protocol::COPY) => shell(protocol::COPY, payload(result_id)?),
        // The calculator's value is recomputed from the expression in the id rather than kept in a
        // per-session table: `Evaluate` is pure, so the reply needs no memory of the query.
        ("calculator", protocol::COPY) => shell(protocol::COPY, calculation_text(result_id)?),
        ("clipboard", protocol::COPY) => {
            shell(protocol::COPY, clipboard_text(result_id, session)?)
        }
        (_, action) => {
            return Err(Unroutable::NoHandler {
                provider: provider.to_string(),
                action: action.to_string(),
            })
        }
    };
    Ok(routing)
}

/// The part of the id after the provider prefix, or an unknown result when there is nothing there:
/// `file:` with no path is not a row this daemon made.
fn payload(result_id: &str) -> Result<String, Unroutable> {
    match protocol::id_payload(result_id) {
        payload if payload.is_empty() => Err(Unroutable::UnknownResult(result_id.to_string())),
        payload => Ok(payload),
    }
}

fn shell(action: &str, arg: String) -> Routing {
    Routing::Shell {
        action: action.to_string(),
        args: vec![arg],
    }
}

/// The value a `calc:` row copies: its display form, `4` or `6.213712 mi`.
fn calculation_text(result_id: &str) -> Result<String, Unroutable> {
    crate::calc::evaluate(&protocol::id_payload(result_id))
        .map(|evaluation| evaluation.display())
        .ok_or_else(|| Unroutable::UnknownResult(result_id.to_string()))
}

/// The text a `clip:` row copies: the entry as it was recorded. An entry the user has deleted
/// since the row was offered is unknown, exactly like an id this session never made.
fn clipboard_text(result_id: &str, session: &Session<'_>) -> Result<String, Unroutable> {
    session
        .clipboard
        .list()
        .into_iter()
        .find(|entry| entry.id == result_id)
        .map(|entry| entry.text)
        .ok_or_else(|| Unroutable::UnknownResult(result_id.to_string()))
}

/// A routing outcome as the `a{sv}` `Activate` returns (contract §2).
pub fn outcome_row(routing: Routing) -> Row {
    match routing {
        Routing::Handled | Routing::Open { .. } => {
            Row::from([("handled".to_string(), Variant::new(true))])
        }
        Routing::Shell { action, args } => Row::from([
            ("handled".to_string(), Variant::new(false)),
            ("shell_action".to_string(), Variant::new(action)),
            ("args".to_string(), Variant::new(args)),
        ]),
    }
}

#[interface(name = "org.gnome.Lumen.Engine")]
impl Engine {
    /// Protocol major version. Read-only.
    #[zbus(property)]
    fn version(&self) -> u32 {
        protocol::PROTOCOL_MAJOR
    }

    /// The provider ids that can answer on this machine. Read-only.
    #[zbus(property)]
    fn capabilities(&self) -> Vec<String> {
        self.registry.capabilities()
    }

    /// Whether copies are being recorded. The property and `ClipboardSetEnabled` act on the same
    /// store, so either spelling of "turn it on" works (contract §2).
    #[zbus(property)]
    fn clipboard_enabled(&self) -> bool {
        self.clipboard.is_enabled()
    }

    #[zbus(property)]
    fn set_clipboard_enabled(&self, enabled: bool) {
        self.clipboard.set_enabled(enabled);
    }

    /// Record the activation (usage learning, ticket 13) and route the action (contract §2).
    ///
    /// The daemon performs the actions it owns — opening a file or a recent document with the
    /// desktop's default application. A shell-only action (`open-url`, `copy`, `reveal`) is not
    /// performed here: the reply carries `handled: false` with `shell_action` and `args`, and the
    /// extension runs it. A result this session never offered is an error; a row whose action has
    /// no handler at all still reports the activation it recorded.
    fn activate(&self, result_id: String, action_id: String) -> zbus::fdo::Result<Row> {
        let session = Session {
            urls: &self.routes,
            clipboard: &self.clipboard,
        };
        let routing = match route(&result_id, &action_id, &session) {
            Ok(routing) => Some(routing),
            // An action with no handler: neither half performs it, so the reply says so and the
            // activation is still recorded.
            Err(Unroutable::NoHandler { .. }) => None,
            Err(Unroutable::UnknownResult(id)) => {
                return Err(zbus::fdo::Error::NotSupported(format!(
                    "Lumen does not know the result `{id}`"
                )));
            }
        };

        // The daemon-owned action happens before the reply, so `handled: true` means it happened.
        // Nothing the shell can do would open a file the launcher could not start, so that failure
        // is an error reply rather than a quiet "nothing to run".
        let mut row = match routing {
            Some(Routing::Open { uri }) => {
                if let Err(reason) = self.opener.open(&uri) {
                    eprintln!("lumen-daemon: could not open {uri}: {reason}");
                    return Err(zbus::fdo::Error::Failed(format!(
                        "Lumen could not open {uri}"
                    )));
                }
                outcome_row(Routing::Handled)
            }
            Some(shell) => outcome_row(shell),
            None => Row::from([("handled".to_string(), Variant::new(false))]),
        };
        row.insert(
            "recorded".to_string(),
            Variant::new(!result_id.is_empty()),
        );
        row.insert("usage".to_string(), Variant::new(self.usage.record(&result_id)));
        Ok(row)
    }

    /// Start a search and return its id. Results arrive as signals.
    ///
    /// `options["injected"]` carries candidate results the extension computed itself; they are
    /// merged with the daemon's providers and ranked together. `options["limit"]` and
    /// `options["offset"]` page the daemon's own paging providers (contract §2, §11).
    fn query(
        &self,
        text: String,
        enabled_providers: Vec<String>,
        options: HashMap<String, OwnedVariant>,
    ) -> String {
        let injected: Vec<ResultItem> = options
            .get("injected")
            .map(|v| protocol::injected_rows(v))
            .unwrap_or_default()
            .iter()
            .filter_map(ResultItem::from_row)
            .collect();
        let limit = protocol::option_usize(&options, protocol::OPTION_LIMIT);
        let offset = protocol::option_usize(&options, protocol::OPTION_OFFSET).unwrap_or(0);

        let number = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("s{number}");
        let job = SearchJob {
            number,
            id: id.clone(),
            request: QueryRequest::new(text, enabled_providers)
                .with_injected(injected)
                .with_paging(limit, offset),
        };
        let _ = self.jobs.send(job);
        id
    }

    /// Ask the shell to summon or dismiss the overlay (ticket 16).
    ///
    /// The daemon cannot render anything: the overlay lives inside `gnome-shell`, so this only
    /// emits `ToggleRequested`, which the extension listens for. It is the one D-Bus entry point
    /// `cli/lumen --toggle` needs, so a user can bind a second shortcut (or a mouse button, or a
    /// launcher entry) to Lumen. Always succeeds; a shell that is not running ignores it.
    async fn toggle(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        let _ = emitter.toggle_requested().await;
    }

    /// Emitted by `Toggle()`. The extension toggles the overlay when it arrives.
    #[zbus(signal)]
    async fn toggle_requested(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn results_changed(
        emitter: &SignalEmitter<'_>,
        search_id: String,
        results: Rows,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn search_finished(
        emitter: &SignalEmitter<'_>,
        search_id: String,
    ) -> zbus::Result<()>;

    // ---- Clipboard history (ticket 10, persistent and encrypted since ticket 11). ----

    /// Record one copied text. `false` while history is off, for empty text, and for text that
    /// looks like a secret, so a caller can tell "not recorded" from "recorded" without an error.
    async fn clipboard_push(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        text: String,
        timestamp_ms: u64,
    ) -> bool {
        let recorded = self.clipboard.push(&text, timestamp_of(timestamp_ms));
        if recorded {
            let _ = emitter.clipboard_changed().await;
        }
        recorded
    }

    /// The history, newest first, as contract §3 rows. Empty while history is off (contract §9).
    fn clipboard_list(&self) -> Rows {
        self.clipboard
            .list()
            .iter()
            .map(|entry| entry_row(entry).row())
            .collect()
    }

    /// Drop one entry by id. `false` when no entry has that id.
    async fn clipboard_delete(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        id: String,
    ) -> bool {
        let deleted = self.clipboard.delete(&id);
        if deleted {
            let _ = emitter.clipboard_changed().await;
        }
        deleted
    }

    /// Drop the whole history.
    async fn clipboard_clear(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        self.clipboard.clear();
        let _ = emitter.clipboard_changed().await;
    }

    /// Turn recording on or off. Same store as the `ClipboardEnabled` property, so the property is
    /// republished here as well.
    async fn clipboard_set_enabled(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        enabled: bool,
    ) {
        self.clipboard.set_enabled(enabled);
        let _ = self.clipboard_enabled_changed(&emitter).await;
        let _ = emitter.clipboard_changed().await;
    }

    /// Emitted whenever the history changes: a recorded copy, a delete, a clear, or history being
    /// switched on or off.
    #[zbus(signal)]
    async fn clipboard_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Contract `t` is unsigned; the store keeps a signed millisecond stamp for the scorer.
fn timestamp_of(timestamp_ms: u64) -> i64 {
    i64::try_from(timestamp_ms).unwrap_or(i64::MAX)
}

/// Own `config.dbus_name`, export the engine and start the search thread.
pub async fn serve(config: Config) -> zbus::Result<Serving> {
    // The history is opened before the name is owned, so a keyring that needs the user (or not
    // having one at all) is settled before the first client can ask.
    let clipboard = store::open_clipboard(config.clipboard);
    // What the user has been activating, read back from the data directory (or held in memory when
    // there is nowhere to write it); shared by the recorder and the scorer.
    let usage = Arc::new(UsageStore::from_env());
    let registry = Arc::new(Registry::with_usage(
        &config.indexer,
        Arc::clone(&clipboard),
        Arc::clone(&usage),
    ));
    let issued = Arc::new(AtomicU64::new(0));
    let (jobs_tx, jobs_rx) = mpsc::channel();

    // `Activate` routes through the same table the web-search rows are recorded in.
    let routes = registry.websearch_routes();

    let engine = Engine {
        registry: Arc::clone(&registry),
        jobs: jobs_tx,
        issued: Arc::clone(&issued),
        clipboard,
        routes,
        // The default-application launcher (contract §19); `LUMEN_OPEN_BIN` overrides it for tests.
        opener: Opener::from_environment(),
        usage,
    };

    let name = config.dbus_name.clone();
    let path = config.object_path.clone();

    // Request the name *after* the object server is up. `DO_NOT_QUEUE` alone: no
    // `REPLACE_EXISTING`, so a second daemon cannot steal the name — it must fail instead.
    let connection = zbus::connection::Builder::session()
        .serve_at(path.clone(), engine)
        .build()
        .await?;
    let reply = connection
        .request_name_with_flags(name.clone(), RequestNameFlags::DO_NOT_QUEUE)
        .await?;
    if !matches!(
        reply,
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner
    ) {
        return Err(zbus::Error::NameTaken);
    }

    let worker_connection = connection.clone();
    thread::Builder::new()
        .name("lumen-search".to_string())
        .spawn(move || search_thread(worker_connection, path, registry, jobs_rx, issued))
        .expect("spawn the search thread");

    Ok(Serving { connection, name })
}

fn search_thread(
    connection: Connection,
    path: String,
    registry: Arc<Registry>,
    jobs: mpsc::Receiver<SearchJob>,
    issued: Arc<AtomicU64>,
) {
    let emitter = match SignalEmitter::new(&connection, path.as_str()) {
        Ok(emitter) => emitter,
        Err(_) => return,
    };

    while let Ok(job) = jobs.recv() {
        if job.number < issued.load(Ordering::SeqCst) {
            continue;
        }

        let results: Rows = registry
            .query(&job.request)
            .into_iter()
            .map(|item| item.row())
            .collect();

        let emitted = zbus::block_on(async {
            emitter.results_changed(job.id.clone(), results).await?;
            emitter.search_finished(job.id).await
        });

        if emitted.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{string_field, SHELL_ACTIONS};

    #[test]
    fn the_default_config_is_the_contract_identity() {
        let config = Config::default();
        assert_eq!(config.dbus_name, "org.gnome.Lumen");
        assert_eq!(config.object_path, "/org/gnome/Lumen");
        assert_eq!(config.indexer, Indexer::None);
        assert_eq!(
            config.clipboard,
            ClipboardMode::Persistent,
            "the daemon persists the history unless it is told not to"
        );
    }

    // ---- `Activate` routing (tickets 08 and 17, contract §2, §19) ----

    fn routes_with(id: &str, url: &str) -> Routes {
        let routes = Routes::new();
        routes.remember(id, url);
        routes
    }

    fn bool_field(row: &Row, key: &str) -> Option<bool> {
        row.get(key).and_then(|value| bool::try_from(value).ok())
    }

    /// Routing against an empty url table and an empty history: what a request that names none of
    /// the session's rows is routed with.
    fn empty() -> (Routes, ClipboardStore) {
        (Routes::new(), ClipboardStore::new())
    }

    #[test]
    fn a_web_row_is_routed_to_the_shell_that_opens_it() {
        let routes = routes_with("web:abc", "https://duckduckgo.com/?q=x");
        let clipboard = ClipboardStore::new();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        let routing = route("web:abc", "open", &session).expect("the row routes");

        assert_eq!(
            routing,
            Routing::Shell {
                action: "open-url".to_string(),
                args: vec!["https://duckduckgo.com/?q=x".to_string()],
            },
            "the daemon hands the url over instead of opening it (contract §2, §9)"
        );
        assert!(
            SHELL_ACTIONS.contains(&"open-url"),
            "and it is a shell action the contract names"
        );
    }

    #[test]
    fn a_file_row_is_opened_by_the_daemon_and_shown_or_copied_by_the_shell() {
        let (routes, clipboard) = empty();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        assert_eq!(
            route("file:/home/me/report.pdf", "open", &session).expect("open routes"),
            Routing::Open {
                uri: "/home/me/report.pdf".to_string(),
            },
            "opening a file is the daemon's: it has the default-application launcher"
        );
        assert_eq!(
            route("file:/home/me/report.pdf", "reveal", &session).expect("reveal routes"),
            Routing::Shell {
                action: "reveal".to_string(),
                args: vec!["/home/me/report.pdf".to_string()],
            },
            "the file manager is the shell's, so `Show in Files` is routed to it"
        );
        assert_eq!(
            route("file:/home/me/report.pdf", "copy", &session).expect("copy routes"),
            Routing::Shell {
                action: "copy".to_string(),
                args: vec!["/home/me/report.pdf".to_string()],
            },
            "only the shell owns the Wayland clipboard"
        );
    }

    #[test]
    fn a_recent_document_is_opened_by_the_daemon() {
        let (routes, clipboard) = empty();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        assert_eq!(
            route("recent:/home/me/notes/report.md", "open", &session).expect("open routes"),
            Routing::Open {
                uri: "/home/me/notes/report.md".to_string(),
            }
        );
    }

    #[test]
    fn a_calculation_is_copied_by_the_shell_at_its_value() {
        let (routes, clipboard) = empty();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        assert_eq!(
            route("calc:2 + 2", "copy", &session).expect("copy routes"),
            Routing::Shell {
                action: "copy".to_string(),
                args: vec!["4".to_string()],
            },
            "the value is recomputed from the expression in the id (story 26)"
        );
        assert_eq!(
            route("calc:10 km in miles", "copy", &session).expect("copy routes"),
            Routing::Shell {
                action: "copy".to_string(),
                args: vec!["6.213712 mi".to_string()],
            }
        );
    }

    #[test]
    fn a_clipboard_entry_is_copied_by_the_shell_at_its_text() {
        let (routes, clipboard) = empty();
        clipboard.set_enabled(true);
        clipboard.push("https://example.com/pricing", 1_000);
        let id = clipboard.list()[0].id.clone();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        assert_eq!(
            route(&id, "copy", &session).expect("copy routes"),
            Routing::Shell {
                action: "copy".to_string(),
                args: vec!["https://example.com/pricing".to_string()],
            },
            "the digest in the id cannot be reversed: the history answers for it (story 46)"
        );

        assert!(
            matches!(
                route("clip:0000000000000000", "copy", &session),
                Err(Unroutable::UnknownResult(_))
            ),
            "an entry the history no longer holds is unknown"
        );
    }

    #[test]
    fn a_result_whose_action_has_no_handler_is_refused() {
        let (routes, clipboard) = empty();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        for (id, provider, action) in [
            ("file:/home/me/report.pdf", "files", "delete"),
            ("calc:2 + 2", "calculator", "open"),
            ("calc:2 + 2", "calculator", "reveal"),
            ("clip:8d1", "clipboard", "open"),
            ("recent:/home/me/report.md", "recent", "copy"),
            ("web:def", "websearch", "reveal"),
            ("app:org.gnome.Nautilus.desktop", "applications", "open"),
            ("action:lock", "actions", "run"),
            ("setting:wifi", "settings", "toggle"),
        ] {
            match route(id, action, &session) {
                Err(Unroutable::NoHandler {
                    provider: named,
                    action: named_action,
                }) => {
                    assert_eq!(named, provider, "{id} belongs to {provider}");
                    assert_eq!(named_action, action);
                }
                other => panic!("`{action}` on {id} must not be routed: {other:?}"),
            }
        }
    }

    #[test]
    fn the_daemon_never_opens_the_url_itself() {
        let routing = Routing::Shell {
            action: protocol::OPEN_URL.to_string(),
            args: vec!["https://duckduckgo.com/?q=x".to_string()],
        };

        let row = outcome_row(routing);

        assert_eq!(bool_field(&row, "handled"), Some(false));
        assert_eq!(
            string_field(&row, "shell_action").as_deref(),
            Some("open-url")
        );
        assert_eq!(
            Vec::<String>::try_from(row["args"].try_clone().expect("cloneable")).expect("as"),
            vec!["https://duckduckgo.com/?q=x".to_string()]
        );
    }

    #[test]
    fn a_handled_outcome_carries_nothing_but_handled() {
        for routing in [Routing::Handled, Routing::Open { uri: "/home/me/x".to_string() }] {
            let row = outcome_row(routing);

            assert_eq!(bool_field(&row, "handled"), Some(true));
            assert!(
                !row.contains_key("shell_action") && !row.contains_key("args"),
                "a performed action has nothing for the shell to do"
            );
        }
    }

    #[test]
    fn a_row_the_session_never_offered_is_refused() {
        let routes = routes_with("web:abc", "https://duckduckgo.com/?q=x");
        let clipboard = ClipboardStore::new();
        let session = Session {
            urls: &routes,
            clipboard: &clipboard,
        };

        for id in ["web:def", "nonsense", "", "file:", "recent:"] {
            assert!(
                matches!(
                    route(id, "open", &session),
                    Err(Unroutable::UnknownResult(_))
                ),
                "`{id}` is nothing this session offered"
            );
        }
        assert!(
            route("web:abc", "copy", &session).is_err(),
            "the web row offers exactly one action"
        );
    }
}
