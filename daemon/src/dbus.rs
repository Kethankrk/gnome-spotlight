//! The `org.gnome.Lumen.Engine` interface, and the search pipeline behind it.
//!
//! `Query` returns a search id straight away and hands the work to the search thread, which
//! emits `ResultsChanged` followed by exactly one `SearchFinished`. Nothing on the D-Bus dispatch
//! thread does provider work.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::{interface, Connection, OwnedVariant};

use crate::protocol::{self, ResultItem, Rows};
use crate::providers::QueryRequest;
use crate::registry::{Indexer, Registry};

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
    clipboard_enabled: AtomicBool,
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

    #[zbus(property)]
    fn clipboard_enabled(&self) -> bool {
        self.clipboard_enabled.load(Ordering::Relaxed)
    }

    #[zbus(property)]
    fn set_clipboard_enabled(&self, enabled: bool) {
        self.clipboard_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Start a search and return its id. Results arrive as signals.
    ///
    /// `options["injected"]` carries candidate results the extension computed itself; they are
    /// merged with the daemon's providers and ranked together.
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

        let number = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("s{number}");
        let job = SearchJob {
            number,
            id: id.clone(),
            request: QueryRequest::new(text, enabled_providers).with_injected(injected),
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
}

/// Own `config.dbus_name`, export the engine and start the search thread.
pub async fn serve(config: Config) -> zbus::Result<Serving> {
    let registry = Arc::new(Registry::with_stubs(&config.indexer));
    let issued = Arc::new(AtomicU64::new(0));
    let (jobs_tx, jobs_rx) = mpsc::channel();

    let engine = Engine {
        registry: Arc::clone(&registry),
        jobs: jobs_tx,
        issued: Arc::clone(&issued),
        clipboard_enabled: AtomicBool::new(false),
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
    }
}
