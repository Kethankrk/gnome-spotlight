//! Recent-document results (user stories 33 and 34, ticket 07).
//!
//! The documents the user opened recently are read from the desktop's own cache by
//! [`crate::recent`], which runs here in the daemon: a toolkit that exposes recent files cannot
//! be imported inside `gnome-shell`, so the list is read on this side of the bus and reaches the
//! shell as ordinary results.
//!
//! Everything that makes a recent document a *result* lives here: the exclusion rules shared with
//! file search, matching, ordering, paging, and the contract §3 row with the `open` action and
//! the §6 preview. Performing that action is activation routing, not search.

use std::path::PathBuf;

use zbus::Variant;

use crate::excludes;
use crate::protocol::{Action, ResultItem, Row};
use crate::providers::files::{folder_label, icon_for};
use crate::providers::{page_size, Provider, QueryRequest};
use crate::ranking;
use crate::recent::{self, RecentEntry};

/// How many recent documents a query returns unless the caller asks for something else: the
/// extension's recent-section cap.
pub const DEFAULT_LIMIT: usize = 4;

/// One recent document as a contract §3 result: `recent:<path>`, the file name as the title, and
/// the §6 preview `{kind:'recent',path,size,mtime,folder}`.
pub fn result_for(entry: &RecentEntry, home: &str) -> ResultItem {
    let path = entry.path.to_string_lossy().into_owned();
    let folder = folder_label(&entry.path, home);

    let mut item = ResultItem::new("recent", format!("recent:{path}"), entry.name.clone());
    item.subtitle = folder.clone();
    item.icon = icon_for(&entry.name, false).to_string();
    // Enter opens the document; that is the only thing a recent row offers (contract §3).
    item.actions = vec![Action::new("open", "Open", "open")];
    item.preview = Row::from([
        ("kind".to_string(), Variant::new("recent")),
        ("path".to_string(), Variant::new(path)),
        ("size".to_string(), Variant::new(entry.size)),
        // `t` on the wire: the schema's timestamps are unsigned.
        ("mtime".to_string(), Variant::new(entry.mtime.max(0) as u64)),
        ("folder".to_string(), Variant::new(folder)),
    ]);
    // The §5 recency bonus follows when the desktop recorded the document as used, not the file's
    // own mtime: a document edited long ago but opened this morning is a recent document.
    item.timestamp = (entry.modified > 0).then_some(entry.modified);
    item
}

/// The `recent` provider.
pub struct Recent {
    /// The cache file to read, or `None` to resolve the user's own cache from the environment on
    /// every query.
    path: Option<PathBuf>,
    home: String,
}

impl Default for Recent {
    fn default() -> Self {
        Self::new()
    }
}

impl Recent {
    /// The provider the daemon serves: the user's own cache, read at query time so that a
    /// document opened while Lumen is running is in the next search.
    pub fn new() -> Self {
        Self {
            path: None,
            home: std::env::var("HOME").unwrap_or_default(),
        }
    }

    /// The same provider pointed at one cache file, so a test can seed the user's documents.
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            home: std::env::var("HOME").unwrap_or_default(),
        }
    }

    /// The same provider with an explicit home directory, so the subtitle is testable.
    pub fn with_home(mut self, home: impl Into<String>) -> Self {
        self.home = home.into();
        self
    }

    fn documents(&self) -> Vec<RecentEntry> {
        match &self.path {
            Some(path) => recent::load(path),
            None => recent::load_user(),
        }
    }
}

impl Provider for Recent {
    fn id(&self) -> &'static str {
        "recent"
    }

    /// The provider is always there: it answers from a per-user file that may hold nothing, which
    /// is a fact about the user's documents, not about this machine's capabilities.
    fn available(&self) -> bool {
        true
    }

    fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        let needle = request.text.trim();
        if needle.is_empty() {
            return Vec::new();
        }

        let mut matching: Vec<(f64, RecentEntry)> = self
            .documents()
            .into_iter()
            // The same rules as file results (user story 34): noise is noise wherever it is
            // found, and a rate-limited check that holds even if the cache reader ever returns
            // one.
            .filter(|entry| !excludes::is_excluded(&entry.path))
            .filter_map(|entry| {
                let quality = ranking::match_quality(&entry.name, needle);
                (quality > 0.0).then_some((quality, entry))
            })
            .collect();

        matching.sort_by(|(best, a), (next, b)| {
            next.partial_cmp(best)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.modified.cmp(&a.modified))
                .then_with(|| a.path.cmp(&b.path))
        });

        matching
            .into_iter()
            .skip(request.offset)
            .take(page_size(request.limit, DEFAULT_LIMIT))
            .map(|(_, entry)| result_for(&entry, &self.home))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::OwnedVariant;

    const HOME: &str = "/home/me";

    fn preview_of(item: &ResultItem) -> std::collections::HashMap<String, OwnedVariant> {
        let row = item.row();
        assert_eq!(
            row["preview"].value_signature().to_string(),
            "a{sv}",
            "the preview is always present and always a{{sv}}"
        );
        std::collections::HashMap::<String, OwnedVariant>::try_from(
            row["preview"].try_clone().expect("a clonable preview"),
        )
        .expect("preview is an a{sv}")
    }

    fn text(field: &OwnedVariant) -> String {
        String::try_from(field.try_clone().expect("clonable")).expect("a string")
    }

    /// A cache file and the documents it names, in a directory of its own.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "lumen-recent-provider-{}-{name}",
                std::process::id()
            ));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).expect("create the fixture directory");
            Self { root }
        }

        /// Write a document and return the `file://` URI the cache should name it by.
        fn document(&self, relative: &str, contents: &[u8]) -> String {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent directory"))
                .expect("create the document's directory");
            std::fs::write(&path, contents).expect("write the document");
            format!("file://{}", path.display())
        }

        /// Write the cache: one bookmark per `(uri, modified)` pair.
        fn cache(&self, bookmarks: &[(&str, &str)]) -> PathBuf {
            let body: Vec<String> = bookmarks
                .iter()
                .map(|(uri, modified)| {
                    format!("<bookmark href=\"{uri}\" modified=\"{modified}\"/>")
                })
                .collect();
            let path = self.root.join("recently-used.xbel");
            std::fs::write(
                &path,
                format!("<xbel version=\"1.0\">{}</xbel>", body.join("")),
            )
            .expect("write the cache");
            path
        }

        /// The provider over a written cache, with the fixture directory as home so the subtitle
        /// reads `~/notes` exactly as it does for file results.
        fn provider(&self, bookmarks: &[(&str, &str)]) -> Recent {
            Recent::with_path(self.cache(bookmarks)).with_home(self.root.display().to_string())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_matching_document_becomes_a_contract_result() {
        let fixture = Fixture::new("one");
        let uri = fixture.document("notes/report.md", b"quarterly report");
        let recent = fixture.provider(&[(&uri, "2026-10-30T08:00:00Z")]);

        let results = recent.query(&QueryRequest::new("report", vec!["recent".to_string()]));

        assert_eq!(results.len(), 1);
        let item = &results[0];
        assert_eq!(item.provider, "recent");
        assert_eq!(
            item.id,
            format!("recent:{}/notes/report.md", fixture.root.display())
        );
        assert_eq!(item.title, "report.md");
        assert_eq!(item.subtitle, "~/notes");
        assert_eq!(item.icon, "text-x-generic");
        assert_eq!(
            item.timestamp,
            Some(1_793_347_200_000),
            "the recorded time feeds the recency bonus"
        );
    }

    #[test]
    fn the_only_action_is_open() {
        let fixture = Fixture::new("actions");
        let uri = fixture.document("notes/report.md", b"x");
        let recent = fixture.provider(&[(&uri, "2026-10-30T08:00:00Z")]);

        let item = &recent.query(&QueryRequest::new("report", Vec::new()))[0];

        let actions: Vec<(String, String, String)> = item
            .actions
            .iter()
            .map(|action| {
                (
                    action.id.clone(),
                    action.label.clone(),
                    action.handler.clone(),
                )
            })
            .collect();
        assert_eq!(
            actions,
            vec![("open".to_string(), "Open".to_string(), "open".to_string())],
            "Enter opens the document"
        );
    }

    #[test]
    fn the_preview_is_the_recent_kind_with_path_size_mtime_and_folder() {
        let fixture = Fixture::new("preview");
        let uri = fixture.document("notes/report.md", b"quarterly report");
        let recent = fixture.provider(&[(&uri, "2026-10-30T08:00:00Z")]);

        let item = &recent.query(&QueryRequest::new("report", Vec::new()))[0];
        let preview = preview_of(item);

        assert_eq!(text(&preview["kind"]), "recent");
        assert_eq!(
            text(&preview["path"]),
            format!("{}/notes/report.md", fixture.root.display())
        );
        assert_eq!(text(&preview["folder"]), "~/notes");
        assert_eq!(text(&preview["kind"]), "recent", "not `file`");
        assert_eq!(preview["size"].value_signature().to_string(), "t");
        assert_eq!(preview["mtime"].value_signature().to_string(), "t");
        assert_eq!(
            u64::try_from(preview["size"].try_clone().unwrap()).unwrap(),
            16
        );
        assert!(
            u64::try_from(preview["mtime"].try_clone().unwrap()).unwrap() > 0,
            "the file's own modification time"
        );
    }

    #[test]
    fn matching_documents_come_back_newest_first() {
        let fixture = Fixture::new("order");
        let older = fixture.document("notes/daily-report-old.md", b"old");
        let newer = fixture.document("notes/report.md", b"new");
        let recent = fixture.provider(&[
            (&older, "2026-10-27T09:00:00Z"),
            (&newer, "2026-10-30T08:00:00Z"),
        ]);

        let results = recent.query(&QueryRequest::new("report", Vec::new()));

        let titles: Vec<&str> = results.iter().map(|item| item.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["report.md", "daily-report-old.md"],
            "equal match quality breaks by the recorded time, newest first"
        );
    }

    #[test]
    fn only_matching_documents_are_returned() {
        let fixture = Fixture::new("matching");
        let report = fixture.document("notes/report.md", b"x");
        let budget = fixture.document("notes/budget.md", b"x");
        let recent = fixture.provider(&[
            (&report, "2026-10-30T08:00:00Z"),
            (&budget, "2026-10-29T08:00:00Z"),
        ]);

        let results = recent.query(&QueryRequest::new("report", Vec::new()));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "report.md");

        assert!(
            recent
                .query(&QueryRequest::new("zzz", Vec::new()))
                .is_empty(),
            "a query that matches nothing returns nothing"
        );
        assert!(
            recent
                .query(&QueryRequest::new("   ", Vec::new()))
                .is_empty(),
            "an empty query is not a search"
        );
    }

    #[test]
    fn the_same_exclusions_as_file_results_apply() {
        let fixture = Fixture::new("exclusions");
        let kept = fixture.document("notes/report.md", b"x");
        let recent = fixture.provider(&[
            (&kept, "2026-10-30T08:00:00Z"),
            (
                &fixture.document(".cache/report.md", b"x"),
                "2026-10-30T08:00:00Z",
            ),
            (
                &fixture.document(".git/report-commits.txt", b"x"),
                "2026-10-30T08:00:00Z",
            ),
            (
                &fixture.document("app/node_modules/report/index.js", b"x"),
                "2026-10-30T08:00:00Z",
            ),
            (
                &fixture.document("app/target/report.log", b"x"),
                "2026-10-30T08:00:00Z",
            ),
        ]);

        let results = recent.query(&QueryRequest::new("report", Vec::new()));

        assert_eq!(
            results.len(),
            1,
            "dot-directories, VCS directories and build noise are excluded with no configuration"
        );
        assert_eq!(results[0].title, "report.md");
    }

    #[test]
    fn a_document_that_is_no_longer_on_disk_is_not_a_result() {
        let fixture = Fixture::new("gone");
        let present = fixture.document("notes/report.md", b"x");
        let recent = fixture.provider(&[
            (&present, "2026-10-30T08:00:00Z"),
            (
                &format!("file://{}/notes/deleted-report.md", fixture.root.display()),
                "2026-10-31T08:00:00Z",
            ),
        ]);

        let results = recent.query(&QueryRequest::new("report", Vec::new()));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "report.md");
    }

    #[test]
    fn a_web_page_is_not_a_recent_document() {
        let fixture = Fixture::new("uri");
        let recent = fixture.provider(&[
            ("https://example.com/report", "2026-10-31T08:00:00Z"),
            ("smb://server/share/report.md", "2026-10-31T08:00:00Z"),
        ]);
        assert!(recent
            .query(&QueryRequest::new("report", Vec::new()))
            .is_empty());
    }

    #[test]
    fn a_machine_with_no_cache_has_no_recent_documents() {
        let recent = Recent::with_path("/nonexistent/recently-used.xbel").with_home(HOME);
        assert!(
            recent.available(),
            "the provider exists; this user has no documents"
        );
        assert!(recent
            .query(&QueryRequest::new("report", Vec::new()))
            .is_empty());
    }

    #[test]
    fn results_are_capped_at_the_section_size_and_paged() {
        let fixture = Fixture::new("paging");
        let documents: Vec<String> = (0..7)
            .map(|n| fixture.document(&format!("notes/report-{n:02}.md"), b"x"))
            .collect();
        let bookmarks: Vec<(String, String)> = documents
            .iter()
            .enumerate()
            .map(|(n, uri)| (uri.clone(), format!("2026-10-{:02}T08:00:00Z", 30 - n)))
            .collect();
        let recent = fixture.provider(
            &bookmarks
                .iter()
                .map(|(uri, modified)| (uri.as_str(), modified.as_str()))
                .collect::<Vec<_>>(),
        );

        let first = recent.query(&QueryRequest::new("report", Vec::new()));
        assert_eq!(first.len(), DEFAULT_LIMIT, "the recent section shows four");

        let second = recent.query(&QueryRequest::new("report", Vec::new()).with_paging(None, 4));
        assert_eq!(second.len(), 3, "the rest of the matches");
        let first_ids: Vec<&str> = first.iter().map(|item| item.id.as_str()).collect();
        for item in &second {
            assert!(
                !first_ids.contains(&item.id.as_str()),
                "{} appears on both pages",
                item.id
            );
        }

        let all = recent.query(&QueryRequest::new("report", Vec::new()).with_paging(Some(100), 0));
        assert_eq!(all.len(), 7, "a caller may ask for more, up to the cap");
    }
}
