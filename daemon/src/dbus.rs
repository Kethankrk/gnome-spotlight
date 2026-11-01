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

use crate::protocol::{self, ResultItem, Row, Rows};
use crate::providers::clipboard::entry_row;
use crate::providers::websearch::{self, Routes};
use crate::providers::QueryRequest;
use crate::registry::{Indexer, Registry};
use crate::store::clipboard::ClipboardStore;
use crate::store::usage::UsageStore;

/// Everything the daemon needs to serve its side of the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The well-known name to own. Owning it is the single-instance lock.
    pub dbus_name: String,
    /// Where the engine interface is exported.
    pub object_path: String,
    /// How to reach LocalSearch/TinySPARQL, if at all.
    pub indexer: Indexer,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dbus_name: protocol::BUS_NAME.to_string(),
            object_path: protocol::OBJECT_PATH.to_string(),
            indexer: Indexer::default(),
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
    /// What the user has been activating, shared with the registry's scorer (ticket 13).
    usage: Arc<UsageStore>,
}

/// What `Activate` does with one (result, action) pair (contract §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routing {
    /// The daemon performed the action itself.
    Handled,
    /// The shell performs it: `shell_action` names it and `args` are its arguments. The daemon
    /// does nothing with them — for `open-url` that is deliberate (contract §9: Lumen makes no
    /// network request of its own; the shell opens the browser).
    Shell { action: String, args: Vec<String> },
}

/// Why an `Activate` cannot be routed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unroutable {
    /// A daemon-owned action whose handler lands with its own ticket (files 05, calculator 06,
    /// clipboard 10). It is never handed to the shell: the shell cannot do it either.
    NoHandler { provider: String, action: String },
    /// No row with that id in this session. The table only holds what this daemon offered.
    UnknownResult(String),
}

impl std::fmt::Display for Unroutable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHandler { provider, action } => write!(
                formatter,
                "Lumen cannot perform `{action}` on a `{provider}` result yet"
            ),
            Self::UnknownResult(id) => write!(formatter, "Lumen does not know the result `{id}`"),
        }
    }
}

/// Route one action on one result (contract §2).
///
/// Daemon-owned actions are performed in the daemon; a shell action is *not* performed here but
/// named in the reply, and the extension runs it. `open-url` is the only shell action so far.
pub fn route(result_id: &str, action_id: &str, routes: &Routes) -> Result<Routing, Unroutable> {
    match protocol::provider_of_id(result_id) {
        Some(websearch::PROVIDER) if action_id == websearch::OPEN_ACTION => {
            match routes.url_of(result_id) {
                Some(url) => Ok(Routing::Shell {
                    action: protocol::OPEN_URL.to_string(),
                    args: vec![url],
                }),
                None => Err(Unroutable::UnknownResult(result_id.to_string())),
            }
        }
        Some(provider) => Err(Unroutable::NoHandler {
            provider: provider.to_string(),
            action: action_id.to_string(),
        }),
        None => Err(Unroutable::UnknownResult(result_id.to_string())),
    }
}

/// A routing outcome as the `a{sv}` `Activate` returns (contract §2).
pub fn outcome_row(routing: Routing) -> Row {
    match routing {
        Routing::Handled => Row::from([("handled".to_string(), Variant::new(true))]),
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
    /// The daemon performs the actions it owns. A shell-only action is not performed here: the
    /// reply carries `handled: false` with `shell_action` and `args`, and the extension runs it.
    /// A result this session never offered is an error; a daemon-owned row whose handler has not
    /// landed yet still reports the activation it recorded.
    fn activate(&self, result_id: String, action_id: String) -> zbus::fdo::Result<Row> {
        let mut row = match route(&result_id, &action_id, &self.routes) {
            Ok(routing) => outcome_row(routing),
            Err(Unroutable::NoHandler { .. }) => {
                Row::from([("handled".to_string(), Variant::new(false))])
            }
            Err(Unroutable::UnknownResult(id)) => {
                return Err(zbus::fdo::Error::NotSupported(format!(
                    "Lumen does not know the result `{id}`"
                )));
            }
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

    // ---- Clipboard history (ticket 10; memory-only, persistence is ticket 11). ----

    /// Record one copied text. `false` while history is off or for empty text, so a caller can tell
    /// "not recorded" from "recorded" without an error.
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
    let clipboard = Arc::new(ClipboardStore::new());
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
    }

    // ---- `Activate` routing (ticket 08, contract §2) ----

    fn routes_with(id: &str, url: &str) -> Routes {
        let routes = Routes::new();
        routes.remember(id, url);
        routes
    }

    fn bool_field(row: &Row, key: &str) -> Option<bool> {
        row.get(key).and_then(|value| bool::try_from(value).ok())
    }

    #[test]
    fn a_web_row_is_routed_to_the_shell_that_opens_it() {
        let routes = routes_with("web:abc", "https://duckduckgo.com/?q=x");

        let routing = route("web:abc", "open", &routes).expect("the row routes");

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
        let row = outcome_row(Routing::Handled);

        assert_eq!(bool_field(&row, "handled"), Some(true));
        assert!(
            !row.contains_key("shell_action") && !row.contains_key("args"),
            "a performed action has nothing for the shell to do"
        );
    }

    #[test]
    fn a_daemon_owned_row_is_never_shell_routed() {
        let routes = Routes::new();

        for (id, provider) in [
            ("file:/home/me/report.pdf", "files"),
            ("calc:2 + 2", "calculator"),
            ("clip:8d1", "clipboard"),
            ("recent:file:///home/me/report.pdf", "recent"),
        ] {
            match route(id, "open", &routes) {
                Err(Unroutable::NoHandler {
                    provider: named,
                    action,
                }) => {
                    assert_eq!(named, provider, "{id} belongs to {provider}");
                    assert_eq!(action, "open");
                }
                other => panic!("{id} must not be routed: {other:?}"),
            }
        }

        assert!(
            route("app:org.gnome.Nautilus.desktop", "open", &routes).is_err(),
            "applications never reach the engine's Activate"
        );
    }

    #[test]
    fn a_row_the_session_never_offered_is_refused() {
        let routes = routes_with("web:abc", "https://duckduckgo.com/?q=x");

        for id in ["web:def", "nonsense", ""] {
            assert!(
                matches!(route(id, "open", &routes), Err(Unroutable::UnknownResult(_))),
                "`{id}` was never offered, so its url is unknown"
            );
        }
        assert!(
            route("web:abc", "copy", &routes).is_err(),
            "the web row offers exactly one action"
        );
    }
}
