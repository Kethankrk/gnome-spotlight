//! The file index seam.
//!
//! `files` results come from the system's local search index (GNOME LocalSearch, backed by
//! libtinysparql). Tests, and machines without an index, get the same behaviour through
//! [`FakeIndex`]; on a machine without the library or without a running endpoint the daemon
//! reports the provider as unavailable rather than failing (contract §9).
//!
//! Everything here is reached through [`FileIndex`], which the `files` provider owns:
//!
//! * [`LocalSearchIndex`] — dlopens `libtinysparql-3.0.so.0` and speaks the supported Sparql
//!   connection API to the `org.freedesktop.LocalSearch3` endpoint. No build-time dependency on
//!   the library exists: its symbols are resolved at run time (see [`localsearch`]).
//! * [`FakeIndex`] — seed-driven, no library at all. The seed format is documented in the
//!   implementation contract (`contract.md`, §11).
//! * [`Unavailable`] — the honest answer when neither can answer.
//!
//! Implementations return at most `limit` hits starting at `offset` in index order, with the
//! default exclusion rules ([`crate::excludes`]) already applied: paging means the same thing to
//! every implementation.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::excludes;
use crate::registry::Indexer;

mod localsearch;

pub use localsearch::LocalSearchIndex;

/// One file or folder the index knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: PathBuf,
    /// The file name the index reported, which is what a query matches against.
    pub name: String,
    /// LocalSearch models folders and files as the same `nfo:FileDataObject` type (verified
    /// against a live endpoint), so folder-ness comes from the filesystem.
    pub is_folder: bool,
    pub size: u64,
    pub mtime_ms: i64,
}

impl FileHit {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            name,
            is_folder: false,
            size: 0,
            mtime_ms: 0,
        }
    }
}

/// A source of file and folder hits.
pub trait FileIndex: Send + Sync + 'static {
    /// At most `limit` hits, starting at `offset` in index order, excluded paths already dropped.
    fn search(&self, query: &str, limit: usize, offset: usize) -> Vec<FileHit>;

    /// Whether this index can answer on the machine it is running on.
    fn available(&self) -> bool;
}

/// An index that cannot answer, and why. The reason is logged once at startup.
pub struct Unavailable {
    reason: String,
}

impl Unavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl FileIndex for Unavailable {
    fn search(&self, _query: &str, _limit: usize, _offset: usize) -> Vec<FileHit> {
        Vec::new()
    }

    fn available(&self) -> bool {
        false
    }
}

/// A hand-seeded index: the same matching, ordering, exclusion and paging rules as a real one,
/// over a fixed list of files. Used by tests and by `--indexer fake:<seed.json>`.
pub struct FakeIndex {
    hits: Vec<FileHit>,
}

impl FakeIndex {
    pub fn new(hits: Vec<FileHit>) -> Self {
        Self { hits }
    }

    /// An index with nothing in it. Still available: a real index can be empty too.
    pub fn empty() -> Self {
        Self { hits: Vec::new() }
    }

    /// Read a seed file. The format (contract §11) is
    /// `{"files": [{"path": "/abs/path", "size": 12, "mtime_ms": 1700000000000, "folder": false}]}`
    /// where only `path` is required.
    pub fn from_seed_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read the seed file {}: {e}", path.display()))?;
        Self::from_seed_json(&text)
            .map_err(|e| format!("cannot use the seed file {}: {e}", path.display()))
    }

    pub fn from_seed_json(json: &str) -> Result<Self, String> {
        let seed: Value =
            serde_json::from_str(json).map_err(|e| format!("it is not valid JSON: {e}"))?;
        let entries = seed
            .get("files")
            .ok_or_else(|| "it has no `files` array".to_string())?
            .as_array()
            .ok_or_else(|| "`files` must be an array".to_string())?;

        let mut hits = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let object = entry
                .as_object()
                .ok_or_else(|| format!("files[{index}] must be an object"))?;
            let raw = object
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("files[{index}].path must be a string"))?;
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(format!("files[{index}].path must be absolute: {raw}"));
            }

            let mut hit = FileHit::new(path);
            hit.is_folder = object
                .get("folder")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            hit.size = object.get("size").and_then(Value::as_u64).unwrap_or(0);
            hit.mtime_ms = object.get("mtime_ms").and_then(Value::as_i64).unwrap_or(0);
            hits.push(hit);
        }

        Ok(Self { hits })
    }
}

impl FileIndex for FakeIndex {
    fn search(&self, query: &str, limit: usize, offset: usize) -> Vec<FileHit> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() || limit == 0 {
            return Vec::new();
        }

        let matching = excludes::filter_excluded(
            self.hits
                .iter()
                .filter(|hit| excludes::quality(&hit.name, &needle) > 0.0)
                .cloned()
                .collect(),
        );

        excludes::rank(matching, &needle)
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect()
    }

    fn available(&self) -> bool {
        true
    }
}

/// Build the index the configuration asks for. Never fails: an index that cannot answer is
/// represented by [`Unavailable`], so the daemon still serves every other provider.
pub fn open(indexer: &Indexer) -> Arc<dyn FileIndex> {
    match indexer {
        Indexer::None => Arc::new(Unavailable::new("no indexer configured (--indexer none)")),
        Indexer::Fake { seed } => match seed {
            None => Arc::new(FakeIndex::empty()),
            Some(path) => match FakeIndex::from_seed_file(path) {
                Ok(index) => Arc::new(index),
                Err(reason) => {
                    eprintln!("lumen-daemon: {reason}; the fake index is empty");
                    Arc::new(Unavailable::new(reason))
                }
            },
        },
        Indexer::LocalSearch => match LocalSearchIndex::open() {
            Ok(index) => Arc::new(index),
            Err(reason) => {
                eprintln!("lumen-daemon: local search index unavailable: {reason}");
                Arc::new(Unavailable::new(reason))
            }
        },
    }
}

/// `file:///home/me/a%20b.txt` → `/home/me/a b.txt`. Only local URIs: a remote one names a file
/// this process cannot open, so it is dropped rather than guessed at.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let (host, path) = rest.split_once('/')?;
    if !host.is_empty() && host != "localhost" {
        return None;
    }
    Some(PathBuf::from(percent_decode(&format!("/{path}"))))
}

/// Parse the ISO 8601 timestamps LocalSearch reports (`2026-10-29T11:12:09.881084Z`) into
/// milliseconds since the epoch. Sub-millisecond digits are truncated.
pub fn parse_iso8601_ms(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once('T')?;

    let mut fields = date.split('-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: u32 = fields.next()?.parse().ok()?;
    let day: u32 = fields.next()?.parse().ok()?;
    if fields.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // LocalSearch writes UTC (`Z`); an explicit offset is honoured when one is present.
    let (clock, offset_minutes) = match time.rfind(&['Z', '+', '-'][..]) {
        Some(at) if time.as_bytes()[at] == b'Z' => (&time[..at], 0),
        Some(at) => (&time[..at], zone_offset_minutes(&time[at..])?),
        None => (time, 0),
    };

    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut fields = clock.split(':');
    let hour: i64 = fields.next()?.parse().ok()?;
    let minute: i64 = fields.next()?.parse().ok()?;
    let second: i64 = fields.next()?.parse().ok()?;
    if fields.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let millis = match fraction.len().min(3) {
        0 => 0,
        1 => fraction[..1].parse::<i64>().ok()? * 100,
        2 => fraction[..2].parse::<i64>().ok()? * 10,
        _ => fraction[..3].parse::<i64>().ok()?,
    };

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    Some(seconds * 1_000 + millis)
}

fn zone_offset_minutes(zone: &str) -> Option<i64> {
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (hours, minutes) = zone[1..].split_once(':').unwrap_or((&zone[1..], "0"));
    Some(sign * (hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?))
}

/// Days since 1970-01-01 from a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 3 <= bytes.len() {
            if let (Some(high), Some(low)) = (hex_value(bytes[at + 1]), hex_value(bytes[at + 2])) {
                out.push(high * 16 + low);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: &str = r#"{
      "files": [
        {"path": "/home/me/notes/report.md", "size": 2048, "mtime_ms": 1700000000000},
        {"path": "/home/me/Projects/lumen", "folder": true},
        {"path": "/home/me/notes/reports-2024.md", "size": 12, "mtime_ms": 1600000000000},
        {"path": "/home/me/.git/report.txt"},
        {"path": "/home/me/app/node_modules/report/index.js"},
        {"path": "/home/me/notes/unrelated.md"}
      ]
    }"#;

    fn seeded() -> FakeIndex {
        FakeIndex::from_seed_json(SEED).expect("the documented seed format parses")
    }

    #[test]
    fn a_seed_yields_the_files_it_names() {
        let hits = seeded().search("report", 10, 0);
        let names: Vec<&str> = hits.iter().map(|hit| hit.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["report.md", "reports-2024.md"],
            "excluded paths are never returned and unrelated files do not match"
        );
        assert_eq!(hits[0].size, 2048);
        assert_eq!(hits[0].mtime_ms, 1_700_000_000_000);
        assert!(!hits[0].is_folder);
    }

    #[test]
    fn a_seeded_folder_is_a_folder_and_matches_by_name() {
        let hits = seeded().search("lumen", 10, 0);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].is_folder);
        assert_eq!(hits[0].path, PathBuf::from("/home/me/Projects/lumen"));
        assert_eq!(hits[0].size, 0, "an unset size defaults to zero");
    }

    #[test]
    fn the_fake_index_pages_and_an_empty_query_returns_nothing() {
        assert_eq!(seeded().search("report", 10, 0).len(), 2);
        assert_eq!(seeded().search("report", 1, 0).len(), 1);
        let second = seeded().search("report", 1, 1);
        assert_eq!(second[0].name, "reports-2024.md");
        assert!(seeded().search("report", 1, 5).is_empty());
        assert!(seeded().search("   ", 10, 0).is_empty());
        assert!(seeded().search("report", 0, 0).is_empty());
    }

    #[test]
    fn a_broken_seed_says_what_is_wrong() {
        assert!(FakeIndex::from_seed_json("not json").is_err());
        assert!(FakeIndex::from_seed_json("{}").is_err());
        assert!(FakeIndex::from_seed_json(r#"{"files": {}}"#).is_err());
        assert!(FakeIndex::from_seed_json(r#"{"files": [{}]}"#).is_err());
        let relative = FakeIndex::from_seed_json(r#"{"files": [{"path": "notes/x.md"}]}"#);
        assert!(relative.is_err(), "a seed path must be absolute");
    }

    #[test]
    fn an_unavailable_index_answers_nothing() {
        let index = Unavailable::new("no library");
        assert!(!index.available());
        assert!(index.search("report", 10, 0).is_empty());
        assert_eq!(index.reason(), "no library");
    }

    #[test]
    fn opening_an_index_follows_the_configuration() {
        assert!(!open(&Indexer::None).available());

        let fake = open(&Indexer::Fake { seed: None });
        assert!(fake.available(), "an empty fake index is still an index");
        assert!(fake.search("anything", 10, 0).is_empty());

        let missing_seed = std::path::PathBuf::from("/nonexistent/lumen-seed.json");
        let broken = open(&Indexer::Fake {
            seed: Some(missing_seed),
        });
        assert!(!broken.available(), "a seed that cannot be read is unavailable");
    }

    #[test]
    fn file_uris_become_local_paths() {
        assert_eq!(
            uri_to_path("file:///home/me/notes/report.md"),
            Some(PathBuf::from("/home/me/notes/report.md"))
        );
        assert_eq!(
            uri_to_path("file://localhost/home/me/a%20b.txt"),
            Some(PathBuf::from("/home/me/a b.txt"))
        );
        assert_eq!(
            uri_to_path("file:///home/me/caf%C3%A9.md"),
            Some(PathBuf::from("/home/me/café.md"))
        );
        assert_eq!(uri_to_path("https://example.com/x"), None);
        assert_eq!(uri_to_path("file://elsewhere.example/x"), None);
    }

    #[test]
    fn localsearch_timestamps_become_epoch_milliseconds() {
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso8601_ms("2026-10-29T11:12:09Z"),
            Some(1_793_272_329_000)
        );
        assert_eq!(
            parse_iso8601_ms("2026-10-29T11:12:09.881084Z"),
            Some(1_793_272_329_881)
        );
        assert_eq!(
            parse_iso8601_ms("2026-10-29T13:12:09+02:00"),
            Some(1_793_272_329_000),
            "an explicit offset is honoured"
        );
        assert_eq!(parse_iso8601_ms("2026-10-29T11:12:09"), Some(1_793_272_329_000));
        assert_eq!(parse_iso8601_ms("nonsense"), None);
        assert_eq!(parse_iso8601_ms("2026-13-29T11:12:09Z"), None);
    }
}
