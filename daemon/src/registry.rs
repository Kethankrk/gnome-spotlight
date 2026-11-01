//! The provider registry: which providers exist, which of them can answer, and one place that
//! turns a [`QueryRequest`] into results.

use std::path::PathBuf;

use crate::protocol::{ResultItem, CAPABILITY_IDS, PROVIDER_IDS};
use crate::providers::{Provider, QueryRequest, Stub};

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
    /// The registry a ticket-01 daemon runs: every provider present, none of them wired up yet.
    pub fn with_stubs(indexer: &Indexer) -> Self {
        let providers = PROVIDER_IDS
            .iter()
            .map(|id| {
                let available = is_available(id, indexer);
                Box::new(Stub::new(id, available)) as Box<dyn Provider>
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

    /// Ask every provider the request wants for results.
    pub fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        self.providers
            .iter()
            .filter(|provider| provider.available() && request.wants(provider.id()))
            .flat_map(|provider| provider.query(request))
            .collect()
    }
}

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
    fn an_enabled_list_selects_the_providers_asked_for() {
        let request = QueryRequest::new("x", vec!["files".to_string()]);
        assert!(request.wants("files"));
        assert!(!request.wants("calculator"));

        let all = QueryRequest::new("x", Vec::new());
        assert!(all.wants("files"));
        assert!(all.wants("calculator"));
    }
}
