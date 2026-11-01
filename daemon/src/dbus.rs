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
use zbus::{interface, Connection, OwnedVariant};

use crate::protocol::{self, ResultItem, Rows};
use crate::providers::clipboard::entry_row;
use crate::providers::QueryRequest;
use crate::registry::{Indexer, Registry};
use crate::store::clipboard::ClipboardStore;
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
    let registry = Arc::new(Registry::new(&config.indexer, Arc::clone(&clipboard)));
    let issued = Arc::new(AtomicU64::new(0));
    let (jobs_tx, jobs_rx) = mpsc::channel();

    let engine = Engine {
        registry: Arc::clone(&registry),
        jobs: jobs_tx,
        issued: Arc::clone(&issued),
        clipboard,
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
}
