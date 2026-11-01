//! The provider registry: which providers exist, which of them can answer, and one place that
//! turns a [`QueryRequest`] into results.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::index;
use crate::protocol::{ResultItem, CAPABILITY_IDS, PROVIDER_IDS};
use crate::providers::{files, Provider, QueryRequest, Stub};
use crate::ranking;

/// How the daemon reaches the file index. The index is behind its own seam so that the daemon
/// can run, and its tests can run, on a machine without an index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Indexer {
    /// No indexer: the `files` capability is absent.
    #[default]
    None,
    /// A test double in place of the real indexer, optionally seeded from a file.
    Fake { seed: Option<PathBuf> },
    /// GNOME LocalSearch / TinySPARQL, loaded at run time. Unavailable, and reported as such,
    /// when the library is missing or no endpoint answers.
    LocalSearch,
}

pub struct Registry {
    providers: Vec<Box<dyn Provider>>,
}

impl Registry {
    /// The registry a daemon runs: every provider present, with `files` backed by whichever index
    /// the configuration selected and the rest still stubs.
    ///
    /// Building it opens the index, which for `LocalSearch` means loading the library and probing
    /// the endpoint on this thread — before the bus name is owned, so the capability is right
    /// before the first client can ask. Later queries run on the search thread (see `dbus.rs`).
    pub fn new(indexer: &Indexer) -> Self {
        let index = index::open(indexer);
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(Stub::new("applications", false)),
            Box::new(files::Files::new(Arc::clone(&index))),
            Box::new(Stub::new("recent", false)),
            Box::new(Stub::new("clipboard", false)),
            Box::new(Stub::new("calculator", true)),
            Box::new(Stub::new("actions", false)),
            Box::new(Stub::new("settings", false)),
            Box::new(Stub::new("websearch", true)),
        ];
        debug_assert_eq!(
            providers.iter().map(|p| p.id()).collect::<Vec<_>>(),
            PROVIDER_IDS.to_vec(),
            "the registry lists the providers in contract order"
        );

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_lists_all_eight_providers() {
        let registry = Registry::new(&Indexer::None);
        assert_eq!(registry.ids(), PROVIDER_IDS.to_vec());
    }

    #[test]
    fn capabilities_omit_what_is_not_available() {
        let without_index = Registry::new(&Indexer::None);
        assert_eq!(
            without_index.capabilities(),
            vec!["calculator".to_string(), "websearch".to_string()]
        );

        let with_index = Registry::new(&Indexer::Fake { seed: None });
        assert_eq!(
            with_index.capabilities(),
            vec![
                "files".to_string(),
                "calculator".to_string(),
                "websearch".to_string()
            ]
        );
    }

    #[test]
    fn an_empty_index_contributes_no_results() {
        let registry = Registry::new(&Indexer::Fake { seed: None });
        let request = QueryRequest::new("nautilus", vec!["files".to_string()]);
        assert!(registry.query(&request).is_empty());
    }

    #[test]
    fn a_seeded_index_feeds_the_files_provider() {
        let seed = std::env::temp_dir().join(format!(
            "lumen-registry-seed-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &seed,
            r#"{"files": [{"path": "/home/me/notes/report.md", "size": 10, "mtime_ms": 1700000000000}]}"#,
        )
        .expect("write a seed");

        let registry = Registry::new(&Indexer::Fake {
            seed: Some(seed.clone()),
        });
        let results = registry.query(&QueryRequest::new("report", vec!["files".to_string()]));
        std::fs::remove_file(&seed).ok();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].provider, "files");
        assert_eq!(results[0].id, "file:/home/me/notes/report.md");
        assert_eq!(results[0].score, 0.20 + 0.8, "base weight plus prefix quality");
    }

    #[test]
    fn without_an_index_the_extension_s_own_results_still_rank() {
        let registry = Registry::new(&Indexer::None);
        let app = ResultItem::new("applications", "app:org.gnome.Nautilus.desktop", "Files");
        let request =
            QueryRequest::new("files", vec!["applications".to_string(), "files".to_string()])
                .with_injected(vec![app]);

        let results = registry.query(&request);
        assert_eq!(results.len(), 1, "the file provider stays silent");
        assert_eq!(results[0].provider, "applications");
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
