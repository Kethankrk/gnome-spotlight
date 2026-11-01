//! The usage counters behind contract §5's `usage_bonus`, keyed by result id.
//!
//! An activation is recorded here and read back by the registry before scoring, so the results the
//! user reaches for most rise in the ranking. The counters outlive the daemon: they live in one
//! small JSON file under the user's data directory, and a store with no file behind it (the
//! in-memory one the tests use) behaves identically except that it forgets on exit.
//!
//! Nothing here is allowed to break a search or fail a D-Bus call: a counter file that is missing,
//! unreadable, corrupt or unwritable degrades to "no counts" / "not persisted on this machine".

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::protocol::ResultItem;

/// How the stored counters are shaped on disk.
const FILE_VERSION: u32 = 1;
/// The file's name inside the data directory.
const FILE_NAME: &str = "usage.json";

/// How many distinct ids are remembered. Counters for results the user has not touched in a long
/// while are dropped so the file cannot grow without bound; the highest counts are kept, and the
/// ranking's own cap is 20 activations anyway.
pub const MAX_ENTRIES: usize = 1000;

/// The path the counters live at, given the caller's `XDG_DATA_HOME` and `HOME`.
///
/// `None` when neither is known: the store then stays in memory, which is the same behaviour as a
/// machine with no writable data directory.
pub fn usage_path(data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = data_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|value| PathBuf::from(value).join(".local/share"))
        })?;
    Some(base.join("lumen").join(FILE_NAME))
}

#[derive(Debug)]
struct Inner {
    /// Result id → number of activations.
    counts: HashMap<String, u32>,
    /// Where the counters are written, if anywhere.
    path: Option<PathBuf>,
    cap: usize,
}

/// Activation counters, shared by the D-Bus dispatch thread (which records activations) and the
/// search thread (which reads them while ranking).
#[derive(Debug)]
pub struct UsageStore {
    inner: Mutex<Inner>,
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageStore {
    /// A store with nothing behind it: every count is forgotten when the process exits. This is
    /// what tests and a machine with no data directory get.
    pub fn new() -> Self {
        Self::with_cap(MAX_ENTRIES)
    }

    /// [`UsageStore::new`] with an explicit entry cap, so pruning is testable.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                counts: HashMap::new(),
                path: None,
                cap,
            }),
        }
    }

    /// A store backed by `path`, with whatever is already in that file. A file that is missing,
    /// unreadable or unparseable is an empty store, never an error.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let counts = load(&path);
        Self {
            inner: Mutex::new(Inner {
                counts,
                path: Some(path),
                cap: MAX_ENTRIES,
            }),
        }
    }

    /// The store the daemon serves: `$LUMEN_USAGE_FILE` when set (a testing seam), else the file
    /// under `$XDG_DATA_HOME`/`$HOME`; the in-memory store when neither is available.
    pub fn from_env() -> Self {
        if let Ok(path) = std::env::var("LUMEN_USAGE_FILE") {
            if path.is_empty() {
                return Self::new();
            }
            return Self::open(PathBuf::from(path));
        }

        let data_home = std::env::var("XDG_DATA_HOME").ok();
        let home = std::env::var("HOME").ok();
        match usage_path(data_home.as_deref(), home.as_deref()) {
            Some(path) => Self::open(path),
            None => Self::new(),
        }
    }

    /// Where the counters are written, if they are written at all.
    pub fn path(&self) -> Option<PathBuf> {
        self.lock().path.clone()
    }

    /// How often `id` has been activated. Zero for a result nobody has activated.
    pub fn count(&self, id: &str) -> u32 {
        self.lock().counts.get(id).copied().unwrap_or(0)
    }

    /// Record one activation of `id` and return its new count.
    ///
    /// An empty id is not a result and is not recorded. The count saturates rather than wrapping,
    /// and a counter file that cannot be written is not an error: the count still rises in memory,
    /// so the rest of the session learns normally.
    pub fn record(&self, id: &str) -> u32 {
        if id.is_empty() {
            return 0;
        }

        let mut inner = self.lock();
        let count = inner.counts.get(id).copied().unwrap_or(0).saturating_add(1);
        inner.counts.insert(id.to_string(), count);
        let cap = inner.cap;
        prune(&mut inner.counts, cap, id);

        let path = inner.path.clone();
        if let Some(path) = path {
            if let Err(e) = save(&path, &inner.counts) {
                eprintln!("lumen-daemon: cannot write the usage counters to {path:?}: {e}");
            }
        }

        count
    }

    /// Give every candidate its activation count, before the scorer looks at them.
    pub fn apply(&self, items: &mut [ResultItem]) {
        let inner = self.lock();
        for item in items {
            item.usage = inner.counts.get(&item.id).copied().unwrap_or(0);
        }
    }

    /// How many distinct results are remembered. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.lock().counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every counter, lowest id first, so a test can state the whole map.
    pub fn counts(&self) -> Vec<(String, u32)> {
        let inner = self.lock();
        let mut counts: Vec<(String, u32)> = inner
            .counts
            .iter()
            .map(|(id, count)| (id.clone(), *count))
            .collect();
        counts.sort();
        counts
    }

    /// A poisoned lock still holds consistent data here (every critical section is infallible), so
    /// recover from it rather than panicking on a D-Bus thread.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Keep the highest `cap` counts, dropping the rest. Ties keep the lexicographically smallest ids,
/// so the stored file is deterministic — but `keep`, the result the user has just activated, is
/// never the one dropped: the id in front of them always keeps its history.
fn prune(counts: &mut HashMap<String, u32>, cap: usize, keep: &str) {
    if counts.len() <= cap {
        return;
    }

    let mut entries: Vec<(String, u32)> = counts
        .iter()
        .map(|(id, count)| (id.clone(), *count))
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let protected = entries.iter().position(|(id, _)| id == keep);
    entries.truncate(cap);
    if let Some(_index) = protected.filter(|index| *index >= cap) {
        entries.pop();
        entries.push((keep.to_string(), counts[keep]));
    }

    counts.clear();
    counts.extend(entries);
}

/// Read the counters from `path`. Anything unreadable, unparseable or nonsensical is dropped
/// silently: a damaged counter file must cost the user nothing but their history of use.
fn load(path: &Path) -> HashMap<String, u32> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
        return HashMap::new();
    };
    let Some(entries) = document.get("counts").and_then(|counts| counts.as_array()) else {
        return HashMap::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let count = u32::try_from(entry.get("count")?.as_u64()?).unwrap_or(u32::MAX);
            (!id.is_empty() && count > 0).then_some((id, count))
        })
        .collect()
}

/// Write the counters to `path`, creating the directory, and leave no half-written file behind.
fn save(path: &Path, counts: &HashMap<String, u32>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let entries: Vec<serde_json::Value> = counts
        .iter()
        .map(|(id, count)| serde_json::json!({"id": id, "count": count}))
        .collect();
    let document = serde_json::json!({"version": FILE_VERSION, "counts": entries});
    let text = serde_json::to_vec(&document).map_err(io::Error::other)?;

    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, &text)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A scratch directory of this test's own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "lumen-usage-{}-{name}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&root).expect("a scratch directory");
            Self(root)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn item(id: &str) -> ResultItem {
        ResultItem::new("applications", id, "Files")
    }

    #[test]
    fn a_fresh_store_has_no_counts() {
        let store = UsageStore::new();
        assert!(store.is_empty());
        assert_eq!(store.count("app:org.gnome.Nautilus.desktop"), 0);
        assert!(store.path().is_none(), "an in-memory store has no file");
    }

    #[test]
    fn recording_counts_up() {
        let store = UsageStore::new();
        assert_eq!(store.record("file:/home/me/notes"), 1);
        assert_eq!(store.record("file:/home/me/notes"), 2);
        assert_eq!(store.count("file:/home/me/notes"), 2);
        assert_eq!(store.count("file:/home/me/other"), 0);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn an_empty_id_is_not_a_result() {
        let store = UsageStore::new();
        assert_eq!(store.record(""), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn a_count_saturates_instead_of_wrapping() {
        let scratch = Scratch::new("saturate");
        let path = scratch.file("usage.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"version":1,"counts":[{{"id":"app:x","count":{}}}]}}"#,
                u32::MAX
            ),
        )
        .expect("write a counter file");

        let store = UsageStore::open(&path);
        assert_eq!(store.count("app:x"), u32::MAX);
        assert_eq!(store.record("app:x"), u32::MAX, "it must not wrap to zero");
    }

    #[test]
    fn counts_survive_a_round_trip_through_the_file() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.file("usage.json");

        {
            let store = UsageStore::open(&path);
            store.record("app:org.gnome.Nautilus.desktop");
            store.record("app:org.gnome.Nautilus.desktop");
            store.record("file:/home/me/notes/report.md");
            assert_eq!(store.len(), 2);
        }

        let reopened = UsageStore::open(&path);
        assert_eq!(
            reopened.counts(),
            vec![
                ("app:org.gnome.Nautilus.desktop".to_string(), 2),
                ("file:/home/me/notes/report.md".to_string(), 1),
            ],
            "a restart keeps what the user has been choosing"
        );
        assert_eq!(reopened.path(), Some(path));
    }

    #[test]
    fn recording_creates_a_missing_directory() {
        let scratch = Scratch::new("missing-dir");
        let path = scratch.file("nested/data/usage.json");

        let store = UsageStore::open(&path);
        assert!(store.is_empty());
        assert_eq!(store.record("app:x"), 1);
        assert!(
            path.is_file(),
            "the counters are written on the first activation"
        );
        assert_eq!(UsageStore::open(&path).count("app:x"), 1);
    }

    #[test]
    fn a_corrupt_or_odd_file_is_an_empty_store_not_a_failure() {
        let scratch = Scratch::new("corrupt");

        for junk in [
            "not json at all",
            "[]",
            r#"{"version":1}"#,
            r#"{"version":1,"counts":"nonsense"}"#,
            r#"{"version":1,"counts":[{"id":"app:x"},{"count":3},{"id":"","count":4},{"id":"app:y","count":0},{"id":"file:/a","count":"many"}]}"#,
        ] {
            let path = scratch.file("junk.json");
            std::fs::write(&path, junk).expect("write junk");

            let store = UsageStore::open(&path);
            assert!(store.is_empty(), "`{junk}` must read as no counts");

            assert_eq!(store.record("app:x"), 1, "and the store still works");
            assert_eq!(store.count("app:x"), 1);
        }
    }

    #[test]
    fn a_count_that_cannot_be_written_is_not_an_error() {
        let scratch = Scratch::new("unwritable");
        let directory = scratch.file("a-directory");
        std::fs::create_dir_all(&directory).expect("a directory where the file should be");

        // The "file" is a directory, so writing it must fail; recording must still count.
        let store = UsageStore::open(&directory);
        assert_eq!(store.record("app:x"), 1);
        assert_eq!(store.count("app:x"), 1);
    }

    #[test]
    fn only_the_most_used_ids_are_kept() {
        let store = UsageStore::with_cap(2);
        store.record("app:often");
        store.record("app:often");
        store.record("app:sometimes");
        store.record("app:sometimes");
        assert_eq!(store.len(), 2, "two fit");

        assert_eq!(store.record("app:new"), 1);

        assert_eq!(store.len(), 2, "the cap holds");
        assert_eq!(
            store.count("app:new"),
            1,
            "the id just activated is never the one dropped"
        );
        assert_eq!(store.count("app:often"), 2, "the most used id stays");
        assert_eq!(
            store.count("app:sometimes"),
            0,
            "the tie was broken to make room"
        );
    }

    #[test]
    fn apply_gives_every_candidate_its_count() {
        let store = UsageStore::new();
        store.record("app:used");
        store.record("app:used");

        let mut items = vec![item("app:used"), item("app:never")];
        store.apply(&mut items);

        assert_eq!(items[0].usage, 2);
        assert_eq!(items[1].usage, 0, "an unknown result scores without usage");
    }

    #[test]
    fn the_counters_are_not_lost_when_they_are_recorded_from_several_threads() {
        let store = Arc::new(UsageStore::new());

        let writers: Vec<_> = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        store.record("app:contended");
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().expect("no thread panics");
        }

        assert_eq!(store.count("app:contended"), 200);
    }

    #[test]
    fn the_path_follows_the_users_data_directory() {
        assert_eq!(
            usage_path(Some("/home/me/data"), Some("/home/me")),
            Some(PathBuf::from("/home/me/data/lumen/usage.json")),
            "XDG_DATA_HOME wins"
        );
        assert_eq!(
            usage_path(None, Some("/home/me")),
            Some(PathBuf::from("/home/me/.local/share/lumen/usage.json")),
            "the XDG default is used when the variable is unset"
        );
        assert_eq!(usage_path(Some(""), None), None, "nothing to write to");
        assert_eq!(usage_path(None, Some("")), None);
        assert_eq!(usage_path(None, None), None);
    }
}
