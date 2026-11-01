//! File and folder results (contract §3, user stories 17–24).
//!
//! The provider owns the search policy — what to ask the index for, what to drop, how much to
//! return — and the index owns the data. That split is what lets the whole of this behaviour be
//! tested without LocalSearch installed.
//!
//! Every hit carries the three actions the ticket asks for: `open` (Enter), `reveal` (the
//! modifier that shows the file in Files) and `copy` (the one that copies its path). Performing
//! them is activation routing, not search; the daemon advertises the descriptors here.

use std::path::Path;
use std::sync::Arc;

use zbus::Variant;

use crate::excludes;
use crate::index::{FileHit, FileIndex};
use crate::protocol::{Action, ResultItem, Row};
use crate::providers::{Provider, QueryRequest};

/// How many file hits a query returns unless the caller asks for something else: the extension's
/// files-section cap, so a broad query such as a single letter cannot flood the list.
pub const DEFAULT_LIMIT: usize = 8;
/// The most a caller can ask for, however large a limit it passes.
pub const MAX_LIMIT: usize = 30;

/// Resolve the requested page size: the caller's limit if it sent one, clamped to [`MAX_LIMIT`],
/// and never zero.
pub fn resolve_limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// The themed icon name for a hit.
pub fn icon_for(name: &str, is_folder: bool) -> &'static str {
    if is_folder {
        return "folder";
    }
    match extension(name).as_str() {
        "pdf" => "application-pdf",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image-x-generic",
        "mp3" | "flac" | "ogg" | "opus" | "wav" => "audio-x-generic",
        "mp4" | "mkv" | "webm" | "mov" => "video-x-generic",
        "zip" | "tar" | "gz" | "xz" | "zst" | "deb" | "rpm" => "package-x-generic",
        _ => "text-x-generic",
    }
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// The containing directory, with the home directory written as `~` — the subtitle a result row
/// shows.
pub fn folder_label(path: &Path, home: &str) -> String {
    let parent = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    abbreviate_home(&parent, home)
}

fn abbreviate_home(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// One hit as a contract §3 result: `file:<path>`, the file name as the title, and the §6 preview
/// with the path, size, modification time and containing folder.
pub fn result_for(hit: &FileHit, home: &str) -> ResultItem {
    let path = hit.path.to_string_lossy().into_owned();
    let title = if hit.name.is_empty() {
        path.clone()
    } else {
        hit.name.clone()
    };

    let mut item = ResultItem::new("files", format!("file:{path}"), title);
    item.subtitle = folder_label(&hit.path, home);
    item.icon = icon_for(&hit.name, hit.is_folder).to_string();
    item.actions = vec![
        Action::new("open", "Open", "open"),
        Action::new("reveal", "Show in Files", "reveal"),
        Action::new("copy", "Copy Path", "copy"),
    ];
    item.preview = Row::from([
        (
            "kind".to_string(),
            Variant::new(if hit.is_folder { "folder" } else { "file" }),
        ),
        ("path".to_string(), Variant::new(path)),
        ("size".to_string(), Variant::new(hit.size)),
        // `t` on the wire: the schema's timestamp is unsigned.
        (
            "mtime".to_string(),
            Variant::new(hit.mtime_ms.max(0) as u64),
        ),
        (
            "folder".to_string(),
            Variant::new(folder_label(&hit.path, home)),
        ),
    ]);
    // Recency comes from the file's own modification time (contract §5).
    item.timestamp = (hit.mtime_ms > 0).then_some(hit.mtime_ms);
    item
}

/// The `files` provider.
pub struct Files {
    index: Arc<dyn FileIndex>,
    home: String,
}

impl Files {
    pub fn new(index: Arc<dyn FileIndex>) -> Self {
        Self {
            index,
            home: std::env::var("HOME").unwrap_or_default(),
        }
    }

    /// The same provider with an explicit home directory, so the subtitle is testable.
    pub fn with_home(index: Arc<dyn FileIndex>, home: impl Into<String>) -> Self {
        Self {
            index,
            home: home.into(),
        }
    }
}

impl Provider for Files {
    fn id(&self) -> &'static str {
        "files"
    }

    fn available(&self) -> bool {
        self.index.available()
    }

    fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        let needle = request.text.trim();
        if needle.is_empty() || !self.index.available() {
            return Vec::new();
        }

        let limit = resolve_limit(request.limit);
        self.index
            .search(needle, limit, request.offset)
            // The index applies the same rules, and must; this is the check that holds even if a
            // future index answers with something it should not have.
            .into_iter()
            .filter(|hit| !excludes::is_excluded(&hit.path))
            .take(limit)
            .map(|hit| result_for(&hit, &self.home))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::FakeIndex;
    use zbus::OwnedVariant;

    const HOME: &str = "/home/me";

    fn seed(paths: &[&str]) -> Arc<dyn FileIndex> {
        let files: Vec<String> = paths
            .iter()
            .map(|path| format!(r#"{{"path": "{path}", "size": 2048, "mtime_ms": 1700000000000}}"#))
            .collect();
        let json = format!(r#"{{"files": [{}]}}"#, files.join(","));
        Arc::new(FakeIndex::from_seed_json(&json).expect("a valid seed"))
    }

    fn provider(paths: &[&str]) -> Files {
        Files::with_home(seed(paths), HOME)
    }

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

    #[test]
    fn a_match_becomes_a_contract_result() {
        let files = provider(&["/home/me/notes/report.md"]);
        let results = files.query(&QueryRequest::new("report", vec!["files".to_string()]));

        assert_eq!(results.len(), 1);
        let item = &results[0];
        assert_eq!(item.id, "file:/home/me/notes/report.md");
        assert_eq!(item.provider, "files");
        assert_eq!(item.title, "report.md");
        assert_eq!(item.subtitle, "~/notes");
        assert_eq!(item.icon, "text-x-generic");
        assert_eq!(item.timestamp, Some(1_700_000_000_000));
    }

    #[test]
    fn every_hit_offers_open_reveal_and_copy() {
        let files = provider(&["/home/me/notes/report.md"]);
        let item = &files.query(&QueryRequest::new("report", Vec::new()))[0];

        let actions: Vec<(String, String, String)> = item
            .actions
            .iter()
            .map(|action| (action.id.clone(), action.label.clone(), action.handler.clone()))
            .collect();
        assert_eq!(
            actions,
            vec![
                ("open".to_string(), "Open".to_string(), "open".to_string()),
                (
                    "reveal".to_string(),
                    "Show in Files".to_string(),
                    "reveal".to_string()
                ),
                (
                    "copy".to_string(),
                    "Copy Path".to_string(),
                    "copy".to_string()
                ),
            ]
        );
    }

    #[test]
    fn the_preview_carries_path_size_mtime_and_folder() {
        let files = provider(&["/home/me/notes/report.md"]);
        let item = &files.query(&QueryRequest::new("report", Vec::new()))[0];
        let preview = preview_of(item);

        assert_eq!(text(&preview["kind"]), "file");
        assert_eq!(text(&preview["path"]), "/home/me/notes/report.md");
        assert_eq!(text(&preview["folder"]), "~/notes");
        assert_eq!(
            u64::try_from(preview["size"].try_clone().unwrap()).unwrap(),
            2048
        );
        assert_eq!(
            preview["size"].value_signature().to_string(),
            "t",
            "size and mtime are the schema's timestamps"
        );
        assert_eq!(preview["mtime"].value_signature().to_string(), "t");
        assert_eq!(
            u64::try_from(preview["mtime"].try_clone().unwrap()).unwrap(),
            1_700_000_000_000
        );
    }

    #[test]
    fn a_folder_says_so_in_its_preview_and_icon() {
        let files = Files::with_home(
            Arc::new(
                FakeIndex::from_seed_json(
                    r#"{"files": [{"path": "/home/me/Projects/lumen", "folder": true}]}"#,
                )
                .unwrap(),
            ),
            HOME,
        );
        let item = &files.query(&QueryRequest::new("lumen", Vec::new()))[0];
        assert_eq!(item.icon, "folder");
        assert_eq!(text(&preview_of(item)["kind"]), "folder");
        assert_eq!(item.subtitle, "~/Projects");
        assert_eq!(item.timestamp, None, "a hit with no mtime has no recency");
    }

    #[test]
    fn an_empty_query_asks_the_index_for_nothing() {
        let files = provider(&["/home/me/notes/report.md"]);
        assert!(files.query(&QueryRequest::new("", Vec::new())).is_empty());
        assert!(files.query(&QueryRequest::new("   ", Vec::new())).is_empty());
    }

    #[test]
    fn an_unavailable_index_reports_no_capability_and_no_results() {
        let files = Files::new(Arc::new(crate::index::Unavailable::new("no library")));
        assert!(!files.available());
        assert!(files.query(&QueryRequest::new("report", Vec::new())).is_empty());
    }

    /// An index that ignores the exclusion rules, to prove the provider's own check holds.
    struct RawIndex(Vec<FileHit>);

    impl FileIndex for RawIndex {
        fn search(&self, _query: &str, limit: usize, offset: usize) -> Vec<FileHit> {
            self.0
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect()
        }

        fn available(&self) -> bool {
            true
        }
    }

    #[test]
    fn an_index_that_returns_noise_does_not_reach_the_user() {
        let index = Arc::new(RawIndex(vec![
            FileHit::new("/home/me/.cache/report.md"),
            FileHit::new("/home/me/app/node_modules/report/index.js"),
            FileHit::new("/home/me/notes/report.md"),
        ]));
        let files = Files::with_home(index, HOME);
        let results = files.query(&QueryRequest::new("report", Vec::new()));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "file:/home/me/notes/report.md");
    }

    #[test]
    fn results_are_capped_and_paged() {
        let paths: Vec<String> = (0..12)
            .map(|n| format!("/home/me/notes/report-{n:02}.md"))
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let files = provider(&paths);

        let first = files.query(&QueryRequest::new("report", Vec::new()));
        assert_eq!(first.len(), DEFAULT_LIMIT, "a broad query cannot flood");

        let second = files.query(&QueryRequest::new("report", Vec::new()).with_paging(None, 8));
        assert_eq!(second.len(), 4, "the rest of the matches");
        let first_ids: Vec<&str> = first.iter().map(|item| item.id.as_str()).collect();
        for item in &second {
            assert!(
                !first_ids.contains(&item.id.as_str()),
                "{} appears on both pages",
                item.id
            );
        }

        let all = files.query(&QueryRequest::new("report", Vec::new()).with_paging(Some(100), 0));
        assert_eq!(all.len(), 12, "a caller may ask for more, up to the cap");
    }

    #[test]
    fn the_requested_page_size_is_clamped() {
        assert_eq!(resolve_limit(None), DEFAULT_LIMIT);
        assert_eq!(resolve_limit(Some(0)), 1);
        assert_eq!(resolve_limit(Some(3)), 3);
        assert_eq!(resolve_limit(Some(1000)), MAX_LIMIT);
    }

    #[test]
    fn icons_follow_the_kind_of_file() {
        assert_eq!(icon_for("notes", true), "folder");
        assert_eq!(icon_for("report.pdf", false), "application-pdf");
        assert_eq!(icon_for("shot.PNG", false), "image-x-generic");
        assert_eq!(icon_for("song.flac", false), "audio-x-generic");
        assert_eq!(icon_for("release.tar.gz", false), "package-x-generic");
        assert_eq!(icon_for("README", false), "text-x-generic");
    }

    #[test]
    fn the_subtitle_shortens_only_the_home_directory() {
        assert_eq!(folder_label(Path::new("/home/me/notes/x.md"), HOME), "~/notes");
        assert_eq!(folder_label(Path::new("/home/me/x.md"), HOME), "~");
        assert_eq!(folder_label(Path::new("/etc/x.conf"), HOME), "/etc");
        assert_eq!(folder_label(Path::new("/home/melanie/x.md"), HOME), "/home/melanie");
        assert_eq!(folder_label(Path::new("x.md"), ""), "");
    }
}
