//! The provider registry: which providers exist, which of them can answer, and one place that
//! turns a [`QueryRequest`] into results.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::{ResultItem, CAPABILITY_IDS, PROVIDER_IDS};
use crate::providers::{calculator::Calculator, Provider, QueryRequest, Stub};
use crate::ranking;

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
    /// The registry a daemon runs: every provider present. The calculator is wired to its real
    /// implementation; the rest are stubs until their ticket lands.
    pub fn with_stubs(indexer: &Indexer) -> Self {
        let providers = PROVIDER_IDS.iter().map(|id| build(id, indexer)).collect();

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

fn is_available(id: &str, indexer: &Indexer) -> bool {
    match id {
        "files" => indexer.available(),
        "calculator" | "websearch" => true,
        _ => false,
    }
}

/// The provider behind a contract id: the calculator is real, the rest are stubs until their
/// ticket lands.
fn build(id: &'static str, indexer: &Indexer) -> Box<dyn Provider> {
    match id {
        "calculator" => Box::new(Calculator::new()),
        other => Box::new(Stub::new(other, is_available(other, indexer))),
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
            vec!["calculator".to_string(), "websearch".to_string()]
        );

        let with_index = Registry::with_stubs(&Indexer::Fake { seed: None });
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
    fn stubs_contribute_no_results() {
        let registry = Registry::with_stubs(&Indexer::Fake { seed: None });
        let request = QueryRequest::new("nautilus", vec!["files".to_string()]);
        assert!(registry.query(&request).is_empty());
    }

    #[test]
    fn an_expression_scores_the_calculators_base_weight_and_is_alone() {
        let registry = Registry::with_stubs(&Indexer::None);

        let results = registry.query_at(&QueryRequest::new("2 + 2", Vec::new()), 0);
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
                    .query_at(&QueryRequest::new(text, Vec::new()), 0)
                    .is_empty(),
                "`{text}` must produce nothing"
            );
        }
    }

    #[test]
    fn an_injected_application_does_not_outrank_a_recognised_expression() {
        let registry = Registry::with_stubs(&Indexer::None);
        let mut app = ResultItem::new("applications", "app:org.gnome.Nautilus.desktop", "Files");
        app.score = 0.30;
        let request = QueryRequest::new("2 + 2", Vec::new()).with_injected(vec![app]);

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
}
