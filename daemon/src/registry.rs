//! The provider registry: which providers exist, which of them can answer, and one place that
//! turns a [`QueryRequest`] into results.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::{ResultItem, CAPABILITY_IDS, PROVIDER_IDS};
use crate::providers::clipboard::{self, ClipboardProvider};
use crate::providers::{Provider, QueryRequest, Stub};
use crate::ranking;
use crate::store::clipboard::ClipboardStore;

/// How the daemon reaches LocalSearch/TinySPARQL. The indexer is behind its own seam so that the
/// daemon can run, and its tests can run, on a machine without an index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Indexer {
    /// No indexer: the `files` capability is absent.
    #[default]
    None,
    /// A test double in place of the real indexer, optionally seeded from a file.
    Fake { seed: Option<PathBuf> },
}

impl Indexer {
    pub fn available(&self) -> bool {
        !matches!(self, Indexer::None)
    }
}

pub struct Registry {
    providers: Vec<Box<dyn Provider>>,
}

impl Registry {
    /// The registry a ticket-01 daemon runs: the clipboard provider wired to a fresh, empty
    /// history, every other provider still a stub.
    pub fn with_stubs(indexer: &Indexer) -> Self {
        Self::new(indexer, Arc::new(ClipboardStore::new()))
    }

    /// The registry the daemon actually serves, sharing one clipboard history with its D-Bus
    /// interface.
    pub fn new(indexer: &Indexer, clipboard: Arc<ClipboardStore>) -> Self {
        let providers = PROVIDER_IDS
            .iter()
            .map(|id| {
                if *id == clipboard::PROVIDER_ID {
                    Box::new(ClipboardProvider::new(Arc::clone(&clipboard))) as Box<dyn Provider>
                } else {
                    let available = is_available(id, indexer);
                    Box::new(Stub::new(id, available)) as Box<dyn Provider>
                }
            })
            .collect();

        Self { providers }
    }

    /// The provider ids the daemon knows, in contract order.
    pub fn ids(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// The `Capabilities` property: the contract ids actually available on this machine.
    pub fn capabilities(&self) -> Vec<String> {
        CAPABILITY_IDS
            .iter()
            .filter(|id| self.is_capable(id))
            .map(|id| (*id).to_string())
            .collect()
    }

    fn is_capable(&self, id: &str) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.id() == id && provider.available())
    }

    /// Ask every provider the request wants for results, merge the extension's injected
    /// candidates, and rank the lot with the single scorer.
    pub fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        self.query_at(request, now_ms())
    }

    /// [`Registry::query`] with an explicit clock, so ranking is testable.
    pub fn query_at(&self, request: &QueryRequest, now_ms: i64) -> Vec<ResultItem> {
        let mut items: Vec<ResultItem> = self
            .providers
            .iter()
            .filter(|provider| provider.available() && request.wants(provider.id()))
            .flat_map(|provider| provider.query(request))
            .collect();
        items.extend(
            request
                .injected
                .iter()
                .filter(|item| request.wants(&item.provider))
                .cloned(),
        );

        for item in &mut items {
            item.score = ranking::score(item, &request.text, now_ms);
        }

        items.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| ranking::provider_rank(&a.provider).cmp(&ranking::provider_rank(&b.provider)))
                .then_with(|| a.title.cmp(&b.title))
        });

        items
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Whether the *stub* provider with this id can answer. Providers that have landed (the clipboard)
/// decide for themselves, so they never appear here.
fn is_available(id: &str, indexer: &Indexer) -> bool {
    match id {
        "files" => indexer.available(),
        "calculator" | "websearch" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_lists_all_eight_providers() {
        let registry = Registry::with_stubs(&Indexer::None);
        assert_eq!(registry.ids(), PROVIDER_IDS.to_vec());
    }

    #[test]
    fn capabilities_omit_what_is_not_available() {
        let without_index = Registry::with_stubs(&Indexer::None);
        assert_eq!(
            without_index.capabilities(),
            vec![
                "clipboard".to_string(),
                "calculator".to_string(),
                "websearch".to_string()
            ],
            "the clipboard provider is always there; without an indexer files is not"
        );

        let with_index = Registry::with_stubs(&Indexer::Fake { seed: None });
        assert_eq!(
            with_index.capabilities(),
            vec![
                "files".to_string(),
                "clipboard".to_string(),
                "calculator".to_string(),
                "websearch".to_string()
            ]
        );
    }

    #[test]
    fn stubs_contribute_no_results() {
        let registry = Registry::with_stubs(&Indexer::Fake { seed: None });
        let request = QueryRequest::new("nautilus", vec!["files".to_string()]);
        assert!(registry.query(&request).is_empty());
    }

    #[test]
    fn the_registry_serves_the_shared_clipboard_history() {
        let store = Arc::new(ClipboardStore::new());
        let registry = Registry::new(&Indexer::None, Arc::clone(&store));
        let request = QueryRequest::new("example", vec!["clipboard".to_string()]);

        let disabled = registry.query(&request);
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0].id, clipboard::DISABLED_ID);

        store.set_enabled(true);
        store.push("https://example.com/pricing", 1_000);
        let enabled = registry.query(&request);
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].title, "https://example.com/pricing");
        assert!(enabled[0].score > 0.0, "the registry scores every result");
    }

    #[test]
    fn an_enabled_list_selects_the_providers_asked_for() {
        let request = QueryRequest::new("x", vec!["files".to_string()]);
        assert!(request.wants("files"));
        assert!(!request.wants("calculator"));

        let all = QueryRequest::new("x", Vec::new());
        assert!(all.wants("files"));
        assert!(all.wants("calculator"));
    }
}
