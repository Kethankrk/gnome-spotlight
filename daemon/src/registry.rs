//! The provider registry: which providers exist, which of them can answer, and one place that
//! turns a [`QueryRequest`] into results.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::index;
use crate::protocol::{ResultItem, CAPABILITY_IDS, PROVIDER_IDS};
use crate::providers::calculator::Calculator;
use crate::providers::clipboard::{self, ClipboardProvider};
use crate::providers::{files, Provider, QueryRequest, Stub};
use crate::ranking;
use crate::store::clipboard::ClipboardStore;
use crate::store::usage::UsageStore;

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
    /// What the user has been activating, keyed by result id (contract §5's usage term).
    usage: Arc<UsageStore>,
}

impl Registry {
    /// The registry the daemon serves: every provider present, `files` backed by whichever index
    /// the configuration selected, the calculator real, the clipboard sharing `clipboard`, and
    /// the rest still stubs until their ticket lands.
    ///
    /// Building it opens the index, which for `LocalSearch` means loading the library and probing
    /// the endpoint on this thread — before the bus name is owned, so the capability is right
    /// before the first client can ask. Later queries run on the search thread (see `dbus.rs`).
    ///
    /// Usage-based ranking comes from [`Registry::with_usage`]; this constructor is for callers
    /// with nothing to remember (tests and one-off builds).
    pub fn new(indexer: &Indexer, clipboard: Arc<ClipboardStore>) -> Self {
        Self::with_usage(indexer, clipboard, Arc::new(UsageStore::new()))
    }

    /// [`Registry::new`] with the usage counters the daemon learns from, so the results the user
    /// activates most rank higher (contract §5, ticket 13).
    pub fn with_usage(
        indexer: &Indexer,
        clipboard: Arc<ClipboardStore>,
        usage: Arc<UsageStore>,
    ) -> Self {
        let index = index::open(indexer);
        let providers: Vec<Box<dyn Provider>> = vec![
            Box::new(Stub::new("applications", false)),
            Box::new(files::Files::new(Arc::clone(&index))),
            Box::new(Stub::new("recent", false)),
            Box::new(ClipboardProvider::new(Arc::clone(&clipboard))),
            Box::new(Calculator::new()),
            Box::new(Stub::new("actions", false)),
            Box::new(Stub::new("settings", false)),
            Box::new(Stub::new("websearch", true)),
        ];
        debug_assert_eq!(
            providers.iter().map(|p| p.id()).collect::<Vec<_>>(),
            PROVIDER_IDS.to_vec(),
            "the registry lists the providers in contract order"
        );

        Self { providers, usage }
    }

    /// A registry with a fresh, empty clipboard history. Convenient for tests.
    pub fn with_stubs(indexer: &Indexer) -> Self {
        Self::new(indexer, Arc::new(ClipboardStore::new()))
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
            // What the user has been choosing weights the score (contract §5), so the counters are
            // read before the scorer runs, never after.
            item.usage = self.usage.count(&item.id);
            item.score = ranking::score(item, &request.text, now_ms);
        }

        items.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    ranking::provider_rank(&a.provider).cmp(&ranking::provider_rank(&b.provider))
                })
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
    fn an_empty_index_contributes_no_results() {
        let registry = Registry::with_stubs(&Indexer::Fake { seed: None });
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

        let registry = Registry::with_stubs(&Indexer::Fake {
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
    fn without_an_index_the_extensions_own_results_still_rank() {
        let registry = Registry::with_stubs(&Indexer::None);
        let app = ResultItem::new("applications", "app:org.gnome.Nautilus.desktop", "Files");
        let request =
            QueryRequest::new("files", vec!["applications".to_string(), "files".to_string()])
                .with_injected(vec![app]);

        let results = registry.query(&request);
        assert_eq!(results.len(), 1, "the file provider stays silent");
        assert_eq!(results[0].provider, "applications");
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
    fn an_expression_scores_the_calculators_base_weight_and_is_alone() {
        let registry = Registry::with_stubs(&Indexer::None);

        let results = registry.query_at(&QueryRequest::new("2 + 2", vec!["calculator".to_string()]), 0);
        assert_eq!(results.len(), 1, "one candidate, and no error row");
        assert_eq!(results[0].provider, "calculator");
        assert_eq!(results[0].id, "calc:2 + 2");
        assert!(
            (results[0].score - 0.40).abs() < 1e-9,
            "base weight 0.40 + no title match, got {}",
            results[0].score
        );
    }

    #[test]
    fn a_query_that_is_not_an_expression_leaves_the_calculator_silent() {
        let registry = Registry::with_stubs(&Indexer::None);
        for text in ["nautilus", "2 +", "abc 123", "10 km"] {
            assert!(
                registry
                    .query_at(&QueryRequest::new(text, vec!["calculator".to_string()]), 0)
                    .is_empty(),
                "`{text}` must produce nothing"
            );
        }
    }

    #[test]
    fn an_injected_application_does_not_outrank_a_recognised_expression() {
        let registry = Registry::with_stubs(&Indexer::None);
        let app = ResultItem::new("applications", "app:org.gnome.Nautilus.desktop", "Files");
        let request = QueryRequest::new(
            "2 + 2",
            vec!["calculator".to_string(), "applications".to_string()],
        )
        .with_injected(vec![app]);

        let results = registry.query_at(&request, 0);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].provider, "calculator",
            "the calculator is the Top Hit for a query that parses"
        );
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

    #[test]
    fn activations_recorded_in_the_usage_store_promote_a_result() {
        let usage = Arc::new(UsageStore::new());
        let registry = Registry::with_usage(
            &Indexer::None,
            Arc::new(ClipboardStore::new()),
            Arc::clone(&usage),
        );

        let candidates = || {
            vec![
                ResultItem::new("applications", "app:org.gnome.Notes.desktop", "Notes"),
                ResultItem::new("files", "file:/home/me/notes-old", "notes-old"),
            ]
        };
        let request = || {
            QueryRequest::new(
                "note",
                vec!["applications".to_string(), "files".to_string()],
            )
            .with_injected(candidates())
        };

        let before = registry.query_at(&request(), 0);
        assert_eq!(
            before[0].provider, "applications",
            "the app's heavier base weight wins first: {before:?}"
        );

        for _ in 0..20 {
            usage.record("file:/home/me/notes-old");
        }

        let after = registry.query_at(&request(), 0);
        assert_eq!(after[0].id, "file:/home/me/notes-old");
        assert_eq!(after[0].usage, 20, "the count reached the candidate");
        assert_eq!(after[1].usage, 0, "an untouched result scores without a bonus");
        assert!(
            (after[0].score - 1.20).abs() < 1e-9,
            "files 0.20 + prefix 0.8 + usage 0.20, got {}",
            after[0].score
        );
        assert!(
            (after[1].score - 1.10).abs() < 1e-9,
            "applications 0.30 + prefix 0.8, got {}",
            after[1].score
        );
    }

    #[test]
    fn an_exact_match_outranks_a_loosely_matching_result_however_often_it_is_used() {
        let usage = Arc::new(UsageStore::new());
        let registry = Registry::with_usage(
            &Indexer::None,
            Arc::new(ClipboardStore::new()),
            Arc::clone(&usage),
        );

        // The file matches only at a word start, and is used far past the scorer's cap.
        for _ in 0..50 {
            usage.record("file:/home/me/My Files Backup");
        }

        let request = QueryRequest::new(
            "files",
            vec!["applications".to_string(), "files".to_string()],
        )
        .with_injected(vec![
            ResultItem::new("applications", "app:org.gnome.Nautilus.desktop", "Files"),
            ResultItem::new("files", "file:/home/me/My Files Backup", "My Files Backup"),
        ]);

        let results = registry.query_at(&request, 0);
        assert_eq!(results[0].provider, "applications");
        assert!((results[0].score - 1.30).abs() < 1e-9);
        assert!(
            (results[1].score - 1.00).abs() < 1e-9,
            "0.20 + 0.6 + the capped 0.20 bonus, got {}",
            results[1].score
        );
    }
}
