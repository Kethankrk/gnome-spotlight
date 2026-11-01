//! The user's recent documents: the `recently-used.xbel` cache, read and parsed on this side.
//!
//! Recent documents come from the desktop's own bookmark cache, which is the same file every
//! GNOME application writes when it opens a document. It is read here — in the daemon, off the
//! shell's main loop — rather than through a toolkit inside `gnome-shell` (user story 33, ticket
//! 07). The parsing is separated from the running system so that the whole of it is testable on a
//! machine with no such file:
//!
//! * [`parse`] is pure: it takes the XML text and an existence probe (which also reports the size
//!   and modification time the preview needs) and returns the documents that are still there;
//! * [`stat`] is the real probe, [`xbel_path_in`] resolves the cache location from the XDG base
//!   directories, and [`load`] reads a cache file from disk.
//!
//! This module deliberately knows nothing about results, ranking or the D-Bus surface: turning a
//! [`RecentEntry`] into a contract §3 row is the provider's job.

use std::path::{Path, PathBuf};

use crate::index::{parse_iso8601_ms, uri_to_path};

/// What the file itself says about itself, as far as this module needs to know. `None` from a
/// probe means the document is gone (or was never on this machine) and the entry is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    pub mtime_ms: i64,
}

/// One document the user opened recently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEntry {
    /// The bookmark's `href`, exactly as the cache wrote it.
    pub uri: String,
    /// The local path that `uri` names.
    pub path: PathBuf,
    /// The file name, which is what a query matches against and what a row shows as its title.
    pub name: String,
    /// The file's size in bytes, from the probe.
    pub size: u64,
    /// The file's own modification time in milliseconds, from the probe.
    pub mtime: i64,
    /// When the desktop last recorded the document as modified/visited, in milliseconds. This is
    /// the ordering key: the newest document is the one the user touched last.
    pub modified: i64,
}

/// Parse a `recently-used.xbel` document.
///
/// Returns the documents it names that `stamp` still finds on disk, newest first. Non-file URIs
/// (a web page, a mail message, a share on another machine) are not documents this daemon can
/// open, so they are dropped rather than guessed at. A cache that is malformed, truncated or not
/// XML at all yields what could be read and never an error: a broken cache must not break the
/// launcher.
pub fn parse(xml: &str, stamp: impl Fn(&Path) -> Option<FileStamp>) -> Vec<RecentEntry> {
    let mut entries: Vec<RecentEntry> = Vec::new();

    for bookmark in bookmarks(xml) {
        let Some(uri) = attribute(&bookmark, "href") else {
            continue;
        };
        let Some(path) = uri_to_path(uri) else {
            continue;
        };
        let Some(stamp) = stamp(&path) else {
            continue;
        };

        entries.push(RecentEntry {
            uri: uri.to_string(),
            name: file_name(&path),
            path,
            size: stamp.size,
            mtime: stamp.mtime_ms,
            modified: recorded_time(&bookmark).unwrap_or(stamp.mtime_ms),
        });
    }

    entries.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    entries
}

/// The files on disk a query needs an answer for.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// When the cache says the document was last used: `modified` is what an application writes when
/// it touches the file, `visited` when it opens it, `added` when the bookmark was first made.
fn recorded_time(bookmark: &[(String, String)]) -> Option<i64> {
    ["modified", "visited", "added"]
        .iter()
        .find_map(|key| attribute(bookmark, key).and_then(parse_iso8601_ms))
}

fn attribute<'a>(bookmark: &'a [(String, String)], key: &str) -> Option<&'a str> {
    bookmark
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// The real existence probe: `None` for anything that is not there, otherwise the size and
/// modification time the preview shows.
pub fn stat(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0);
    Some(FileStamp {
        size: metadata.len(),
        mtime_ms,
    })
}

/// The cache location, per the XDG base directory specification: `$XDG_DATA_HOME`, or
/// `$HOME/.local/share` when that is unset or empty. Split out from the environment read so the
/// rule is testable.
pub fn xbel_path_in(xdg_data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = non_empty(xdg_data_home) {
        return Some(PathBuf::from(dir).join("recently-used.xbel"));
    }
    Some(PathBuf::from(non_empty(home)?).join(".local/share/recently-used.xbel"))
}

/// The user's cache file on this machine, or `None` when neither variable says where home is.
pub fn xbel_path() -> Option<PathBuf> {
    xbel_path_in(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Read a cache file. A file that is missing, unreadable or not text is simply no recent
/// documents. Exclusion rules are the provider's policy, not this module's: everything the cache
/// names that is still on disk comes back.
pub fn load(path: &Path) -> Vec<RecentEntry> {
    match std::fs::read_to_string(path) {
        Ok(xml) => parse(&xml, stat),
        Err(_) => Vec::new(),
    }
}

/// Read the user's own cache file.
pub fn load_user() -> Vec<RecentEntry> {
    match xbel_path() {
        Some(path) => load(&path),
        None => Vec::new(),
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// The attributes of every `<bookmark>` element, in document order.
///
/// A deliberately small reader: the cache is one flat list of bookmarks and the only parts of the
/// format that matter are the attributes on those elements. Comments, CDATA, the XML declaration
/// and every other element (including `bookmark:application`, whose name merely starts with
/// `bookmark`) are skipped. Reading stops at the first construct that does not close, which is
/// what a half-written cache looks like.
fn bookmarks(xml: &str) -> Vec<Vec<(String, String)>> {
    let bytes = xml.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;

    while let Some(open) = find_byte(bytes, at, b'<') {
        let rest = &xml[open..];

        // Comments, CDATA sections and the prolog are skipped whole, so a bookmark that is only
        // written about is not a document. One that never closes is a broken file: stop there and
        // keep what was read.
        if let Some((closing, from)) = skip_marker(rest) {
            match find_str(rest, closing, from) {
                Some(end) => {
                    at = open + end + closing.len();
                    continue;
                }
                None => break,
            }
        }

        let Some(close) = tag_end(bytes, open) else {
            break;
        };
        let tag = &xml[open + 1..close];
        at = close + 1;

        if tag.starts_with('/') {
            continue;
        }
        let (name, rest) = split_name(tag);
        if name != "bookmark" {
            continue;
        }
        if let Some(attributes) = attributes(rest) {
            found.push(attributes);
        }
    }

    found
}

/// The marker that closes a construct that is not an element, for a construct that opens one.
fn skip_marker(rest: &str) -> Option<(&'static str, usize)> {
    if rest.starts_with("<!--") {
        Some(("-->", "<!".len()))
    } else if rest.starts_with("<![CDATA[") {
        Some(("]]>", "<![CDATA[".len()))
    } else if rest.starts_with("<?") {
        Some(("?>", "<?".len()))
    } else if rest.starts_with("<!") {
        Some((">", "<!".len()))
    } else {
        None
    }
}

/// The index of the `>` that closes the tag opened at `open`, or `None` when the tag (or a quoted
/// attribute value inside it) never closes.
fn tag_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut quote = None;
    let mut at = open + 1;
    while at < bytes.len() {
        match (quote, bytes[at]) {
            (Some(open_quote), byte) if byte == open_quote => quote = None,
            (Some(_), _) => {}
            (None, byte @ (b'"' | b'\'')) => quote = Some(byte),
            (None, b'>') => return Some(at),
            (None, _) => {}
        }
        at += 1;
    }
    None
}

fn split_name(tag: &str) -> (&str, &str) {
    let end = tag
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .unwrap_or(tag.len());
    (&tag[..end], &tag[end..])
}

/// The `name="value"` pairs of a tag, with XML entities decoded. `None` when the tag is malformed:
/// an unquoted or unterminated value.
fn attributes(text: &str) -> Option<Vec<(String, String)>> {
    let bytes = text.as_bytes();
    let mut pairs = Vec::new();
    let mut at = 0;

    loop {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() || bytes[at] == b'/' {
            return Some(pairs);
        }

        let name_start = at;
        while at < bytes.len() && bytes[at] != b'=' && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let name = &text[name_start..at];

        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            return None;
        }
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let quote = match bytes.get(at) {
            Some(open @ (b'"' | b'\'')) => *open,
            _ => return None,
        };
        at += 1;

        let value_start = at;
        while at < bytes.len() && bytes[at] != quote {
            at += 1;
        }
        if at >= bytes.len() {
            return None;
        }
        pairs.push((name.to_string(), decode_entities(&text[value_start..at])));
        at += 1;
    }
}

/// The five XML entities, plus numeric character references, which is everything an XBEL
/// attribute value can hold.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match tail.find(';').filter(|end| *end <= 10) {
            Some(end) => match entity(&tail[1..end]) {
                Some(decoded) => {
                    out.push(decoded);
                    rest = &tail[end + 1..];
                }
                None => {
                    out.push('&');
                    rest = &tail[1..];
                }
            },
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => match name.strip_prefix('#')? {
            hex if hex.starts_with(['x', 'X']) => {
                char::from_u32(u32::from_str_radix(&hex[1..], 16).ok()?)
            }
            decimal => char::from_u32(decimal.parse().ok()?),
        },
    }
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|byte| *byte == needle)
        .map(|at| at + from)
}

fn find_str(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    haystack[from..].find(needle).map(|at| at + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache that names every case this module has to cope with: a well-formed document, an
    /// application's nested metadata (whose element name starts with `bookmark`), a web page, a
    /// document that is no longer on disk, and a percent-encoded path.
    const CACHE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0"
      xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks"
      xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info">
  <bookmark href="file:///home/me/notes/budget.md" added="2026-10-01T09:00:00Z" modified="2026-10-29T11:12:09Z" visited="2026-10-29T11:12:09Z">
    <info>
      <metadata owner="http://freedesktop.org">
        <mime:mime-type type="text/markdown"/>
        <bookmark:applications>
          <bookmark:application name="gedit" exec="&apos;gedit %u&apos;" modified="2026-10-29T11:12:09Z"/>
        </bookmark:applications>
      </metadata>
    </info>
  </bookmark>
  <bookmark href="file:///home/me/notes/report.md" modified="2026-10-30T08:00:00Z"/>
  <bookmark href="https://example.com/notes" modified="2026-11-01T00:00:00Z"/>
  <bookmark href="file:///home/me/notes/gone.md" modified="2026-10-28T07:30:00Z"/>
  <bookmark href="file:///home/me/notes/caf%C3%A9%20notes.md" modified="2026-10-27T09:00:00Z"/>
</xbel>
"#;

    /// The probe: a stamp for the paths the test says are on disk, `None` for every other path.
    fn on_disk<'a>(entries: &'a [(&'a str, u64, i64)]) -> impl Fn(&Path) -> Option<FileStamp> + 'a {
        move |path: &Path| {
            entries
                .iter()
                .find(|(known, _, _)| Path::new(known) == path)
                .map(|(_, size, mtime)| FileStamp {
                    size: *size,
                    mtime_ms: *mtime,
                })
        }
    }

    fn paths(entries: &[RecentEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| entry.path.display().to_string())
            .collect()
    }

    #[test]
    fn a_well_formed_cache_lists_the_documents_newest_first() {
        let probe = on_disk(&[
            ("/home/me/notes/budget.md", 2048, 1_700_000_000_000),
            ("/home/me/notes/report.md", 512, 1_700_000_100_000),
            ("/home/me/notes/café notes.md", 64, 1_700_000_200_000),
        ]);

        let entries = parse(CACHE, probe);

        assert_eq!(
            paths(&entries),
            vec![
                "/home/me/notes/report.md",
                "/home/me/notes/budget.md",
                "/home/me/notes/café notes.md",
            ],
            "newest first; the web page and the document that is gone are not documents"
        );

        let budget = &entries[1];
        assert_eq!(budget.uri, "file:///home/me/notes/budget.md");
        assert_eq!(budget.name, "budget.md");
        assert_eq!(budget.size, 2048);
        assert_eq!(budget.mtime, 1_700_000_000_000, "the file's own mtime");
        assert_eq!(
            budget.modified, 1_793_272_329_000,
            "the time the desktop recorded for the document"
        );

        let encoded = &entries[2];
        assert_eq!(
            encoded.path,
            PathBuf::from("/home/me/notes/café notes.md"),
            "a percent-encoded path is decoded"
        );
        assert_eq!(encoded.name, "café notes.md");
    }

    #[test]
    fn only_local_file_uris_are_recent_documents() {
        let xml = r#"<xbel version="1.0">
          <bookmark href="https://example.com/notes"/>
          <bookmark href="smb://server/share/report.md"/>
          <bookmark href="trash:///report.md"/>
          <bookmark href="mailto:someone@example.com"/>
          <bookmark href="file://elsewhere.example/home/me/report.md"/>
          <bookmark href="file:///home/me/notes/report.md"/>
        </xbel>"#;

        let entries = parse(
            xml,
            on_disk(&[("/home/me/notes/report.md", 1, 1_700_000_000_000)]),
        );

        assert_eq!(
            paths(&entries),
            vec!["/home/me/notes/report.md"],
            "a URI this daemon cannot open is dropped, and a remote file:// host is not local"
        );
    }

    #[test]
    fn a_document_that_is_no_longer_on_disk_is_skipped() {
        let xml = r#"<xbel><bookmark href="file:///home/me/notes/gone.md" modified="2026-10-28T07:30:00Z"/></xbel>"#;
        assert!(parse(xml, on_disk(&[])).is_empty());
    }

    #[test]
    fn the_ordering_stamp_falls_back_to_visited_then_added_then_the_file() {
        let xml = r#"<xbel>
          <bookmark href="file:///home/me/a.md" visited="2026-10-30T08:00:00Z" added="2026-10-01T09:00:00Z"/>
          <bookmark href="file:///home/me/b.md" added="2026-10-28T07:30:00Z"/>
          <bookmark href="file:///home/me/c.md"/>
        </xbel>"#;
        let probe = on_disk(&[
            ("/home/me/a.md", 1, 1_600_000_000_000),
            ("/home/me/b.md", 1, 1_600_000_000_000),
            ("/home/me/c.md", 1, 1_600_000_000_000),
        ]);

        let entries = parse(xml, probe);

        assert_eq!(
            paths(&entries),
            vec!["/home/me/a.md", "/home/me/b.md", "/home/me/c.md"]
        );
        assert_eq!(entries[0].modified, 1_793_347_200_000, "visited");
        assert_eq!(
            entries[1].modified, 1_793_172_600_000,
            "added, since nothing visited it"
        );
        assert_eq!(
            entries[2].modified, 1_600_000_000_000,
            "with no recorded time at all, the file's own mtime decides the order"
        );
    }

    #[test]
    fn equal_recorded_times_order_by_path_so_the_list_is_stable() {
        let xml = r#"<xbel>
          <bookmark href="file:///home/me/b.md" modified="2026-10-30T08:00:00Z"/>
          <bookmark href="file:///home/me/a.md" modified="2026-10-30T08:00:00Z"/>
        </xbel>"#;
        let probe = on_disk(&[("/home/me/a.md", 1, 1), ("/home/me/b.md", 1, 1)]);
        assert_eq!(
            paths(&parse(xml, probe)),
            vec!["/home/me/a.md", "/home/me/b.md"]
        );
    }

    #[test]
    fn an_entity_in_an_attribute_is_decoded() {
        let xml = r#"<xbel><bookmark href="file:///home/me/rock&amp;roll/caf&#233;.md"/></xbel>"#;
        let entries = parse(xml, on_disk(&[("/home/me/rock&roll/café.md", 7, 3)]));
        assert_eq!(paths(&entries), vec!["/home/me/rock&roll/café.md"]);
        assert_eq!(entries[0].size, 7);
    }

    #[test]
    fn a_bookmark_inside_a_comment_is_not_a_document() {
        let xml = r#"<xbel><!-- <bookmark href="file:///home/me/notes/report.md"/> --><bookmark href="file:///home/me/notes/real.md"/></xbel>"#;
        let probe = on_disk(&[
            ("/home/me/notes/report.md", 1, 1),
            ("/home/me/notes/real.md", 2, 2),
        ]);

        let entries = parse(xml, probe);

        assert_eq!(paths(&entries), vec!["/home/me/notes/real.md"]);
    }

    #[test]
    fn a_bookmark_inside_a_cdata_section_is_not_a_document_either() {
        let xml = r#"<xbel><metadata><![CDATA[<bookmark href="file:///home/me/notes/report.md"/>]]></metadata><bookmark href="file:///home/me/notes/real.md"/></xbel>"#;
        let probe = on_disk(&[
            ("/home/me/notes/report.md", 1, 1),
            ("/home/me/notes/real.md", 2, 2),
        ]);
        assert_eq!(paths(&parse(xml, probe)), vec!["/home/me/notes/real.md"]);
    }

    #[test]
    fn single_quoted_attributes_are_read_too() {
        let xml = "<xbel><bookmark href='file:///home/me/notes/report.md' modified='2026-10-30T08:00:00Z'/></xbel>";
        let entries = parse(xml, on_disk(&[("/home/me/notes/report.md", 3, 9)]));
        assert_eq!(paths(&entries), vec!["/home/me/notes/report.md"]);
        assert_eq!(entries[0].modified, 1_793_347_200_000);
    }

    #[test]
    fn malformed_xml_yields_nothing_rather_than_an_error() {
        let probe = on_disk(&[("/home/me/notes/report.md", 1, 1)]);
        for xml in [
            "",
            "not xml at all",
            "<?xml version=\"1.0\"",
            "<xbel><bookmark href=\"file:///home/me/notes/report.md\"",
            "<xbel><bookmark href=\"file:///home/me/notes/report.md></xbel>",
            "<xbel><bookmark href=file:///home/me/notes/report.md></xbel>",
            "<xbel><!-- <bookmark href=\"file:///home/me/notes/report.md\"/>",
        ] {
            assert!(
                parse(xml, &probe).is_empty(),
                "{xml:?} must not produce a document"
            );
        }
    }

    #[test]
    fn a_broken_bookmark_does_not_lose_the_ones_after_it() {
        let xml = r#"<xbel><bookmark href=file:///home/me/notes/broken.md><bookmark href="file:///home/me/notes/report.md" modified="2026-10-30T08:00:00Z"/></xbel>"#;
        let probe = on_disk(&[
            ("/home/me/notes/broken.md", 1, 1),
            ("/home/me/notes/report.md", 1, 1),
        ]);

        assert_eq!(paths(&parse(xml, probe)), vec!["/home/me/notes/report.md"]);
    }

    #[test]
    fn an_empty_cache_has_no_documents() {
        assert!(parse("<xbel version=\"1.0\"></xbel>", on_disk(&[])).is_empty());
    }

    #[test]
    fn the_cache_path_follows_the_xdg_base_directory_spec() {
        assert_eq!(
            xbel_path_in(Some("/data"), Some("/home/me")),
            Some(PathBuf::from("/data/recently-used.xbel"))
        );
        assert_eq!(
            xbel_path_in(None, Some("/home/me")),
            Some(PathBuf::from("/home/me/.local/share/recently-used.xbel"))
        );
        assert_eq!(
            xbel_path_in(Some(""), Some("/home/me")),
            Some(PathBuf::from("/home/me/.local/share/recently-used.xbel")),
            "an empty XDG_DATA_HOME is not a directory"
        );
        assert_eq!(xbel_path_in(None, Some("")), None);
        assert_eq!(xbel_path_in(None, None), None);
    }

    #[test]
    fn loading_a_cache_stats_the_files_it_names() {
        let fixture = Fixture::new("load");
        let document = fixture.document("notes/report.md", b"hello");
        let cache = fixture.cache(&format!(
            "<xbel><bookmark href=\"file://{}\" modified=\"2026-10-30T08:00:00Z\"/></xbel>",
            document.display()
        ));

        let entries = load(&cache);

        assert_eq!(paths(&entries), vec![document.display().to_string()]);
        assert_eq!(entries[0].name, "report.md");
        assert_eq!(entries[0].size, 5);
        assert!(entries[0].mtime > 0, "the mtime comes from the filesystem");
        assert_eq!(entries[0].modified, 1_793_347_200_000);
    }

    #[test]
    fn a_cache_that_cannot_be_read_is_simply_no_recent_documents() {
        let fixture = Fixture::new("unreadable");
        assert!(load(Path::new("/nonexistent/recently-used.xbel")).is_empty());
        assert!(
            load(&fixture.binary()).is_empty(),
            "a file that is not text"
        );
    }

    /// A throw-away directory holding documents and a cache file, removed with the test.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("lumen-recent-{}-{name}", std::process::id()));
            std::fs::remove_dir_all(&root).ok();
            std::fs::create_dir_all(&root).expect("create the fixture directory");
            Self { root }
        }

        fn document(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent directory"))
                .expect("create the document's directory");
            std::fs::write(&path, contents).expect("write the document");
            path
        }

        fn cache(&self, xml: &str) -> PathBuf {
            let path = self.root.join("recently-used.xbel");
            std::fs::write(&path, xml).expect("write the cache");
            path
        }

        fn binary(&self) -> PathBuf {
            let path = self.root.join("not-text.xbel");
            std::fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).expect("write a binary file");
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
