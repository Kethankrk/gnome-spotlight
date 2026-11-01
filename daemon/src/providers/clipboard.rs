//! The clipboard provider: clipboard-history entries as results.
//!
//! Matching is left to the shared scorer (contract §5) like every other provider — this module only
//! turns entries into contract §3 rows and, while history is off, into the one row that says so.
//! An off history is never an error (contract §9).

use std::sync::Arc;

use zbus::Variant;

use crate::protocol::{Action, ResultItem, Row};
use crate::providers::{Provider, QueryRequest};
use crate::ranking;
use crate::store::clipboard::{ClipEntry, ClipboardStore};

/// This provider's contract id.
pub const PROVIDER_ID: &str = "clipboard";

/// The row shown while history is off, so an empty clipboard section is never mistaken for a bug.
pub const DISABLED_ID: &str = "clip:disabled";
const DISABLED_TITLE: &str = "Clipboard history is off";
const DISABLED_SUBTITLE: &str = "Copied text is only recorded once you turn it on";

/// The action every entry carries. `Activate` performs it (see the ticket handoff note).
const COPY: (&str, &str, &str) = ("copy", "Copy", "copy");

/// Entries from the shared in-memory history (ticket 10; ticket 11 makes it persistent).
pub struct ClipboardProvider {
    store: Arc<ClipboardStore>,
}

impl ClipboardProvider {
    pub fn new(store: Arc<ClipboardStore>) -> Self {
        Self { store }
    }
}

impl Provider for ClipboardProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    /// The provider always exists: with history off it answers with the disabled row.
    fn available(&self) -> bool {
        true
    }

    fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        if !self.store.is_enabled() {
            return vec![disabled_row()];
        }

        let needle = request.text.trim();
        if needle.is_empty() {
            return Vec::new();
        }

        self.store
            .list()
            .into_iter()
            .filter(|entry| ranking::match_quality(&entry.text, needle) > 0.0)
            .map(|entry| entry_row(&entry))
            .collect()
    }
}

/// One history entry as a contract §3 result. Compact preview per contract §6.
pub fn entry_row(entry: &ClipEntry) -> ResultItem {
    let mut item = ResultItem::new(PROVIDER_ID, entry.id.clone(), entry.text.clone());
    item.actions = vec![Action::new(COPY.0, COPY.1, COPY.2)];
    item.preview = Row::from([
        ("kind".to_string(), Variant::new("clipboard".to_string())),
        ("text".to_string(), Variant::new(entry.text.clone())),
        (
            "timestamp".to_string(),
            Variant::new(u64::try_from(entry.timestamp_ms).unwrap_or(0)),
        ),
    ]);
    item.timestamp = Some(entry.timestamp_ms);
    item
}

/// The single row that replaces the list while history is off: a normal result, not an error.
pub fn disabled_row() -> ResultItem {
    let mut item = ResultItem::new(PROVIDER_ID, DISABLED_ID, DISABLED_TITLE);
    item.subtitle = DISABLED_SUBTITLE.to_string();
    item.preview = Row::from([
        ("kind".to_string(), Variant::new("text".to_string())),
        ("text".to_string(), Variant::new(DISABLED_TITLE.to_string())),
    ]);
    item
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(store: Arc<ClipboardStore>) -> ClipboardProvider {
        ClipboardProvider::new(store)
    }

    fn request(text: &str) -> QueryRequest {
        QueryRequest::new(text, vec![PROVIDER_ID.to_string()])
    }

    #[test]
    fn history_off_yields_exactly_the_disabled_row() {
        let store = Arc::new(ClipboardStore::new());
        let rows = provider(Arc::clone(&store)).query(&request("anything"));

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, DISABLED_ID);
        assert_eq!(rows[0].provider, PROVIDER_ID);
        assert!(rows[0].title.contains("off"));
        assert_eq!(
            crate::protocol::string_field(&rows[0].preview, "kind").as_deref(),
            Some("text")
        );
    }

    #[test]
    fn history_on_matches_entries_with_the_scorer() {
        let store = Arc::new(ClipboardStore::new());
        store.set_enabled(true);
        store.push("https://example.com/pricing", 1_000);
        store.push("an unrelated note", 2_000);

        let rows = provider(Arc::clone(&store)).query(&request("example"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "https://example.com/pricing");
        assert_eq!(rows[0].id, crate::store::clipboard::entry_id("https://example.com/pricing"));
        assert_eq!(rows[0].actions[0].handler, "copy");
        assert_eq!(rows[0].timestamp, Some(1_000));
        assert_eq!(
            crate::protocol::string_field(&rows[0].preview, "kind").as_deref(),
            Some("clipboard")
        );
        assert_eq!(
            crate::protocol::string_field(&rows[0].preview, "text").as_deref(),
            Some("https://example.com/pricing")
        );

        assert!(provider(store).query(&request("zzz")).is_empty());
    }

    #[test]
    fn an_empty_query_matches_nothing_even_with_history_on() {
        let store = Arc::new(ClipboardStore::new());
        store.set_enabled(true);
        store.push("anything", 1_000);
        assert!(provider(store).query(&request("  ")).is_empty());
    }
}
