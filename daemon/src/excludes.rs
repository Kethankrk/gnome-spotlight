//! The file-search policy: what a query asks the index for, how a hit matches in process, and
//! what is never shown.
//!
//! Three things live here, all of them pure so they can be tested without an index:
//!
//! * the **exclusion rules** (user story 23: "dot-directories, VCS directories and other noise
//!   are excluded without configuring anything"), applied in process by every index and mirrored
//!   as a FILTER hint for the index;
//! * the **query builder** — the SPARQL a LocalSearch endpoint is asked for a page of hits, plus
//!   a name-only fallback for endpoints that reject the content-matching form;
//! * the **matcher** — the same match-quality vocabulary the scorer uses (`ranking::match_quality`)
//!   applied to a file name, so a fake index and a real one rank alike.

use std::path::{Component, Path};

use crate::index::FileHit;
use crate::ranking;

/// Directory names that are noise whatever the project is: version-control metadata and the two
/// build directories a developer's home is full of. Hidden entries — any path component starting
/// with `.` — are noise too, which already covers `.git`, `.hg` and `.svn`; they are named here
/// as well so the rule and the index-side hint state them explicitly.
pub const EXCLUDED_DIRS: [&str; 5] = [".git", ".hg", ".svn", "node_modules", "target"];

/// Whether a path is noise: a dot-entry anywhere in it, or a component named like a build/VCS
/// directory. The check is on components, not on the string, so a file called `target.md` and a
/// directory called `targets` both survive.
pub fn is_excluded(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => {
            let name = name.to_string_lossy();
            name.starts_with('.') || EXCLUDED_DIRS.contains(&name.as_ref())
        }
        _ => false,
    })
}

/// Drop the excluded paths, keeping the order of the rest.
pub fn filter_excluded(hits: Vec<FileHit>) -> Vec<FileHit> {
    hits.into_iter()
        .filter(|hit| !is_excluded(&hit.path))
        .collect()
}

/// The same rules as a SPARQL FILTER over a file URI, so the index can skip noise before paging.
/// It is a hint, not the rule: every implementation still checks [`is_excluded`] in process.
pub fn exclusion_filter(uri_var: &str) -> String {
    let mut terms = vec![format!("!fn:contains({uri_var}, \"/.\")")];
    terms.extend(
        EXCLUDED_DIRS
            .iter()
            .map(|dir| format!("!fn:contains({uri_var}, \"/{dir}\")")),
    );
    format!("FILTER ({})", terms.join(" && "))
}

/// Escape a needle for a double-quoted SPARQL literal.
pub fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' | '\t' => out.push(' '),
            // A control character would end the literal in a way SPARQL does not accept.
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Order hits best-match-first, then by path so the order is stable. Hits that do not match at
/// all are dropped. Match quality is the scorer's own vocabulary (`ranking::match_quality`), so a
/// file result is ranked on the same scale as every other provider's.
pub fn rank(hits: Vec<FileHit>, needle: &str) -> Vec<FileHit> {
    let mut ranked: Vec<(f64, FileHit)> = hits
        .into_iter()
        .filter_map(|hit| {
            let score = ranking::match_quality(&hit.name, needle);
            (score > 0.0).then_some((score, hit))
        })
        .collect();
    ranked.sort_by(|(a, hit_a), (b, hit_b)| {
        b.partial_cmp(a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| hit_a.path.cmp(&hit_b.path))
    });
    ranked.into_iter().map(|(_, hit)| hit).collect()
}

/// A page request: the needle, and how many hits the caller wants from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileQuery {
    needle: String,
    limit: usize,
    offset: usize,
}

/// The columns every file query selects, in the order the cursor maps them by name.
const PROJECTION: &str = "?file ?name ?size ?mtime";
/// Size and modification time are optional: a directory or an unextracted file may lack them.
const OPTIONALS: &str =
    "OPTIONAL { ?file nfo:fileSize ?size } OPTIONAL { ?file nfo:fileLastModified ?mtime }";

impl FileQuery {
    /// The needle is case-folded and trimmed: LocalSearch matches case-insensitively and a
    /// trailing space is never what the user meant.
    pub fn new(text: &str, limit: usize, offset: usize) -> Self {
        Self {
            needle: text.trim().to_lowercase(),
            limit,
            offset,
        }
    }

    pub fn needle(&self) -> &str {
        &self.needle
    }

    /// Nothing to ask for: an empty query must not return the whole index.
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    fn page(&self) -> String {
        format!("LIMIT {} OFFSET {}", self.limit, self.offset)
    }

    fn name_branch(&self) -> String {
        format!(
            "{{ ?file a nfo:FileDataObject ; nfo:fileName ?name . \
             FILTER (fn:contains(fn:lower-case(?name), \"{}\")) }}",
            escape_literal(&self.needle)
        )
    }

    fn content_branch(&self) -> String {
        // `fts:match` on the *resource* uses the endpoint's full-text index: it covers the file
        // name and any indexed text content, and answers in milliseconds. The obvious-looking form
        // — `?file nie:plainTextContent ?content . ?content fts:match "…"` — is catastrophic:
        // it matches nothing where the indexer has not extracted text, and the planner scans the
        // whole index for it, which took ~3s per keystroke (measured) and starved every search.
        format!(
            "{{ ?file fts:match \"{}\" ; nfo:fileName ?name }}",
            escape_literal(&self.needle)
        )
    }

    /// The query a LocalSearch endpoint is asked: hits by name, plus files whose indexed content
    /// matches (user story 18 — where the indexer has covered them). Each UNION branch repeats
    /// its own triple patterns; Tracker rejects a bare FILTER as a branch.
    pub fn sparql(&self) -> String {
        format!(
            "SELECT DISTINCT {PROJECTION} WHERE {{ {} UNION {} {OPTIONALS} {} }} ORDER BY ?name {}",
            self.name_branch(),
            self.content_branch(),
            exclusion_filter("?file"),
            self.page()
        )
    }

    /// The fallback: name matches only. Used when the content form is rejected or fails, so a
    /// version of the endpoint that cannot answer it still returns name hits.
    pub fn name_only_sparql(&self) -> String {
        format!(
            "SELECT DISTINCT {PROJECTION} WHERE {{ {} {OPTIONALS} {} }} ORDER BY ?name {}",
            self.name_branch(),
            exclusion_filter("?file"),
            self.page()
        )
    }

    /// The availability probe: any indexed file at all proves the endpoint answers.
    pub fn probe_sparql() -> &'static str {
        "SELECT ?file WHERE { ?file a nfo:FileDataObject } LIMIT 1"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hit(path: &str) -> FileHit {
        FileHit::new(PathBuf::from(path))
    }

    #[test]
    fn dot_entries_and_noise_directories_are_excluded() {
        for path in [
            "/home/me/.git/config",
            "/home/me/repo/.svn/entries",
            "/home/me/repo/.hg/store",
            "/home/me/app/node_modules/left-pad/index.js",
            "/home/me/app/target/debug/lumen-daemon",
            "/home/me/.cache/thumbnails/large.png",
            "/home/me/notes/.draft.md",
        ] {
            assert!(is_excluded(&PathBuf::from(path)), "{path} must be noise");
        }
    }

    #[test]
    fn ordinary_paths_survive_the_exclusion_rules() {
        for path in [
            "/home/me/notes/todo.md",
            "/home/me/Projects/spotlight/README.md",
            "/home/me/targets/release.md",
            "/home/me/notes/target.md",
            "/home/me/Projects/esp32-learning",
        ] {
            assert!(!is_excluded(&PathBuf::from(path)), "{path} must survive");
        }
    }

    #[test]
    fn filtering_keeps_the_order_of_what_survives() {
        let hits = vec![
            hit("/home/me/a.md"),
            hit("/home/me/.git/a.md"),
            hit("/home/me/b.md"),
            hit("/home/me/node_modules/a.md"),
        ];
        let kept: Vec<PathBuf> = filter_excluded(hits)
            .into_iter()
            .map(|h| h.path)
            .collect();
        assert_eq!(
            kept,
            vec![PathBuf::from("/home/me/a.md"), PathBuf::from("/home/me/b.md")]
        );
    }

    #[test]
    fn the_index_side_filter_names_every_rule() {
        let filter = exclusion_filter("?file");
        assert!(filter.starts_with("FILTER ("), "{filter}");
        assert!(filter.contains("?file"), "{filter}");
        assert!(filter.contains("\"/.\""), "dot entries: {filter}");
        for dir in EXCLUDED_DIRS {
            assert!(filter.contains(&format!("/{dir}")), "{dir} missing: {filter}");
        }
    }

    #[test]
    fn a_needle_cannot_break_out_of_a_sparql_literal() {
        assert_eq!(escape_literal(r#"he said "hi""#), r#"he said \"hi\""#);
        assert_eq!(escape_literal(r"back\slash"), r"back\\slash");
        assert_eq!(escape_literal("two\nlines"), "two lines");

        let query = FileQuery::new("a\"b\\c", 5, 0).sparql();
        assert!(query.contains(r#"\"b\\c"#), "{query}");
        assert!(!query.contains(r#""a"b"#), "{query}");
    }

    #[test]
    fn the_query_pages_and_orders() {
        let query = FileQuery::new("  Report ", 8, 16);
        assert_eq!(query.needle(), "report", "trimmed and case-folded");

        for sparql in [query.sparql(), query.name_only_sparql()] {
            assert!(sparql.contains("LIMIT 8 OFFSET 16"), "{sparql}");
            assert!(sparql.contains("ORDER BY ?name"), "{sparql}");
            assert!(sparql.contains("SELECT DISTINCT ?file ?name ?size ?mtime"), "{sparql}");
            assert!(sparql.contains("fn:contains(fn:lower-case(?name), \"report\")"), "{sparql}");
            assert!(sparql.contains("FILTER ("), "exclusion hint: {sparql}");
        }
    }

    #[test]
    fn only_the_full_query_asks_the_index_for_content() {
        let query = FileQuery::new("budget", 8, 0);
        assert!(query.sparql().contains("fts:match \"budget\""));
        assert!(
            !query.name_only_sparql().contains("fts:match"),
            "{}",
            query.name_only_sparql()
        );

        // The fast form: `fts:match` on the resource. The `nie:plainTextContent` form scanned the
        // whole index (seconds per query) and returned nothing where no text was extracted.
        assert!(
            !query.sparql().contains("nie:plainTextContent"),
            "the content branch must use the full-text index, not a content-property scan: {}",
            query.sparql()
        );
    }

    #[test]
    fn ranking_is_best_match_first_and_stable() {
        let hits = vec![
            hit("/home/me/notes/my-report.md"),    // word start
            hit("/home/me/notes/report.md"),       // prefix
            hit("/home/me/notes/daily-report.md"), // word start
            hit("/home/me/notes/zzz.md"),          // no match
            hit("/home/me/notes/reporting.md"),    // prefix
        ];
        let ranked: Vec<String> = rank(hits, "report")
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(
            ranked,
            vec![
                "report.md",
                "reporting.md",
                "daily-report.md",
                "my-report.md"
            ],
            "prefix before word start, path order inside a tie, and the miss is gone"
        );
    }
}
