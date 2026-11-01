//! The provider seam.
//!
//! Every result source — the daemon's own (files, recent, clipboard, calculator, websearch) and
//! the ones the extension computes and injects — is reached through [`Provider`]. Tests inject
//! fakes here instead of standing up LocalSearch or a clipboard store.

pub mod files;

use crate::protocol::ResultItem;

/// What a provider is asked for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryRequest {
    /// The raw query text, as typed.
    pub text: String,
    /// Provider ids the caller wants results from; empty means "no restriction beyond the
    /// registry's own availability".
    pub enabled: Vec<String>,
    /// Candidate results the extension computed itself (applications, system actions, setting
    /// toggles). The registry ranks these alongside the daemon's own providers.
    pub injected: Vec<ResultItem>,
    /// How many results a paging provider should return for this request, when the caller said.
    pub limit: Option<usize>,
    /// Where in the provider's own ordering to start. Paging is per provider: the extension
    /// pages the file list without re-running the whole search.
    pub offset: usize,
}

impl QueryRequest {
    pub fn new(text: impl Into<String>, enabled: Vec<String>) -> Self {
        Self {
            text: text.into(),
            enabled,
            injected: Vec::new(),
            limit: None,
            offset: 0,
        }
    }

    pub fn with_injected(mut self, injected: Vec<ResultItem>) -> Self {
        self.injected = injected;
        self
    }

    /// Ask for one page: at most `limit` results (the provider's default when `None`), starting
    /// at `offset`.
    pub fn with_paging(mut self, limit: Option<usize>, offset: usize) -> Self {
        self.limit = limit;
        self.offset = offset;
        self
    }

    /// Whether this request wants results from `provider`.
    pub fn wants(&self, provider: &str) -> bool {
        self.enabled.is_empty() || self.enabled.iter().any(|id| id == provider)
    }
}

/// A source of results.
pub trait Provider: Send + Sync + 'static {
    /// The contract id of this provider (one of [`crate::protocol::PROVIDER_IDS`]).
    fn id(&self) -> &'static str;

    /// Whether this provider can answer at runtime, given the machine it is running on.
    fn available(&self) -> bool;

    /// Results for `request`, in any order: the registry ranks them.
    fn query(&self, request: &QueryRequest) -> Vec<ResultItem>;
}

/// A provider with no data source yet. Every later ticket replaces one of these.
pub struct Stub {
    id: &'static str,
    available: bool,
}

impl Stub {
    pub fn new(id: &'static str, available: bool) -> Self {
        Self { id, available }
    }
}

impl Provider for Stub {
    fn id(&self) -> &'static str {
        self.id
    }

    fn available(&self) -> bool {
        self.available
    }

    fn query(&self, _request: &QueryRequest) -> Vec<ResultItem> {
        Vec::new()
    }
}
