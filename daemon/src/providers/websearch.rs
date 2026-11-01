//! The `websearch` provider: one "Search the web for …" row, offered only when the query matched
//! nothing strongly (ticket 08, story 30).
//!
//! Lumen never makes the request. The row's id is `web:<sha1(url)>` (contract §3) — one-way by
//! construction — so the url itself is remembered in this session's [`Routes`] table, and
//! `Activate` hands it back to the shell as `open-url` (contract §2). Opening it is the shell's
//! job: only gnome-shell knows the user's default browser.
//!
//! The engine template is the shared schema's `search-engine` key, read through the `gsettings`
//! CLI (which resolves `GSETTINGS_SCHEMA_DIR` exactly as the daemon's own process would). When
//! GSettings cannot answer — no schema installed, no `gsettings`, an unusable value — the row
//! falls back to [`DEFAULT_ENGINE`]: a missing schema is never an error (contract §9).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::protocol::{Action, ResultItem, SCHEMA_ID};
use crate::providers::{Provider, QueryRequest};
use crate::ranking;

/// The contract id of this provider.
pub const PROVIDER: &str = "websearch";

/// The engine the row uses when GSettings cannot answer (contract §8).
pub const DEFAULT_ENGINE: &str = "https://duckduckgo.com/?q=%s";

/// The shared schema's key holding the engine template.
pub const ENGINE_KEY: &str = "search-engine";

/// Where the query goes in the template.
pub const PLACEHOLDER: &str = "%s";

/// The action the row offers, and the shell action the daemon routes it to (contract §2).
pub const OPEN_ACTION: &str = "open";

/// The match quality from which a row counts as a *strong* match (contract §5: word start).
///
/// A word start is the weakest match that still reads as "you meant this row": exact and prefix
/// matches are stronger, substring and subsequence matches are weaker, and a query that only
/// matches somewhere inside a title is precisely the case where a web search is still wanted.
pub const STRONG_MATCH_QUALITY: f64 = 0.6;

/// How many rows the session table keeps routable. The table only has to cover the results an
/// overlay can be showing; past this size it is dropped and refilled.
pub const MAX_ROUTES: usize = 256;

/// The environment variable the shared schema is resolved through (contract §8).
const SCHEMA_DIR_ENV: &str = "GSETTINGS_SCHEMA_DIR";
/// The reader is a CLI invocation, not a linked library: no GLib dependency in the daemon.
const GSETTINGS: &str = "gsettings";
const SCHEMA_DIR_FLAG: &str = "--schemadir";

/// Percent-encode `text` for use as a query-string value: everything outside the unreserved set
/// becomes `%XX` over its UTF-8 bytes.
pub fn percent_encode(text: &str) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

/// The url the row opens: the query, trimmed and percent-encoded, in the template's placeholder.
pub fn build_url(template: &str, query: &str) -> String {
    template.replacen(PLACEHOLDER, &percent_encode(query.trim()), 1)
}

/// The template to build with: the configured one, or [`DEFAULT_ENGINE`] when there is nothing
/// usable — absent, blank, or without a `%s` to put the query in.
pub fn template_or_default(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|template| !template.is_empty() && template.contains(PLACEHOLDER))
        .unwrap_or(DEFAULT_ENGINE)
        .to_string()
}

/// SHA-1 (RFC 3174) of `input`, lowercase hex.
///
/// Contract §3 asks for the url's hash in the row's id, and all it does is name a row stably and
/// opaquely, so the digest is computed here instead of taking a dependency for it.
pub fn sha1_hex(input: &str) -> String {
    use std::fmt::Write;

    let mut state: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];

    let mut message = input.as_bytes().to_vec();
    let bit_length = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut words = [0u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[index * 4],
                block[index * 4 + 1],
                block[index * 4 + 2],
                block[index * 4 + 3],
            ]);
        }
        for index in 16..80 {
            words[index] = (words[index - 3]
                ^ words[index - 8]
                ^ words[index - 14]
                ^ words[index - 16])
                .rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut digest = String::with_capacity(40);
    for word in state {
        let _ = write!(digest, "{word:08x}");
    }
    digest
}

/// The single row this provider offers: contract §3, `{}` preview (contract §6), one shell action.
pub fn row(query: &str, url: &str) -> ResultItem {
    let mut item = ResultItem::new(
        PROVIDER,
        format!("web:{}", sha1_hex(url)),
        format!("Search the web for {query}"),
    );
    item.actions = vec![Action::new(OPEN_ACTION, "Search", "shell")];
    item
}

/// Whether `query` already found something worth showing, in which case a web search is noise.
///
/// Two things count: a title that matches at a word start or better, and the calculator, which
/// only answers when the query is a recognised expression or unit conversion (contract §5) — that
/// is an answer, not a failed search. The web row itself never counts.
pub fn has_strong_match(results: &[ResultItem], query: &str) -> bool {
    let needle = query.trim();
    if needle.is_empty() {
        return false;
    }

    results.iter().any(|item| {
        item.provider == "calculator"
            || (item.provider != PROVIDER
                && ranking::match_quality(&item.title, needle) >= STRONG_MATCH_QUALITY)
    })
}

/// Whether the fallback row is wanted at all: a real query that matched nothing strongly.
pub fn should_offer(results: &[ResultItem], query: &str) -> bool {
    !query.trim().is_empty() && !has_strong_match(results, query)
}

/// Runs one program and returns its stdout, or `None` when it could not run or failed.
pub type CommandRunner = Arc<dyn Fn(&str, &[String]) -> Option<String> + Send + Sync>;

/// Where the row's engine template comes from.
#[derive(Clone)]
pub struct EngineSetting {
    schema_dir: Option<PathBuf>,
    run: CommandRunner,
}

impl EngineSetting {
    /// The daemon's own reader: the shared schema is resolved through `GSETTINGS_SCHEMA_DIR`
    /// (contract §8), which the installer sets to the extension's `schemas` directory.
    pub fn from_environment() -> Self {
        Self::with_runner(
            std::env::var_os(SCHEMA_DIR_ENV).map(PathBuf::from),
            Arc::new(run_command),
        )
    }

    /// The same reader over one schema directory, for callers that know it.
    pub fn reading_schema(dir: impl Into<PathBuf>) -> Self {
        Self::with_runner(Some(dir.into()), Arc::new(run_command))
    }

    /// A reader over any command runner, so the plumbing is testable without GSettings.
    pub fn with_runner(schema_dir: Option<PathBuf>, run: CommandRunner) -> Self {
        Self { schema_dir, run }
    }

    /// The `search-engine` value the schema holds, or `None` when GSettings cannot answer.
    pub fn schema_value(&self) -> Option<String> {
        let mut args: Vec<String> = Vec::new();
        if let Some(dir) = &self.schema_dir {
            args.push(SCHEMA_DIR_FLAG.to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
        args.push("get".to_string());
        args.push(SCHEMA_ID.to_string());
        args.push(ENGINE_KEY.to_string());

        parse_gvariant_string(&(self.run)(GSETTINGS, &args)?)
    }

    /// The template the row is built with: the setting, or the contract default.
    pub fn template(&self) -> String {
        template_or_default(self.schema_value().as_deref())
    }
}

/// `gsettings get` prints a string as a GVariant literal: `'…'`, with `\'` and `\\` escaped
/// inside. Anything that is not such a literal is no answer at all.
pub fn parse_gvariant_string(output: &str) -> Option<String> {
    let inner = output.trim().strip_prefix('\'')?.strip_suffix('\'')?;

    let mut value = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            value.push(characters.next()?);
        } else {
            value.push(character);
        }
    }
    Some(value)
}

fn run_command(program: &str, args: &[String]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The urls this daemon has offered, keyed by result id.
///
/// `web:<sha1(url)>` cannot be turned back into a url, so the row's url is kept here between the
/// search that offered it and the `Activate` that routes it (contract §2).
#[derive(Clone, Default)]
pub struct Routes {
    urls: Arc<Mutex<HashMap<String, String>>>,
}

impl Routes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&self, result_id: &str, url: &str) {
        let mut urls = self.urls.lock().unwrap_or_else(PoisonError::into_inner);
        if urls.len() >= MAX_ROUTES && !urls.contains_key(result_id) {
            urls.clear();
        }
        urls.insert(result_id.to_string(), url.to_string());
    }

    pub fn url_of(&self, result_id: &str) -> Option<String> {
        self.urls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(result_id)
            .cloned()
    }
}

impl std::fmt::Debug for Routes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Routes")
            .field("urls", &self.urls.lock().map(|urls| urls.len()))
            .finish()
    }
}

/// The provider itself. The registry consults it *after* every other provider, and only when
/// [`should_offer`] says the row is wanted, so the engine setting is read for a query that
/// dead-ended rather than for every keystroke.
#[derive(Clone)]
pub struct WebSearch {
    engine: EngineSetting,
    routes: Routes,
}

impl WebSearch {
    pub fn new(engine: EngineSetting) -> Self {
        Self {
            engine,
            routes: Routes::new(),
        }
    }

    /// The same table `Activate` routes through.
    pub fn routes(&self) -> Routes {
        self.routes.clone()
    }
}

impl Provider for WebSearch {
    fn id(&self) -> &'static str {
        PROVIDER
    }

    fn available(&self) -> bool {
        true
    }

    fn query(&self, request: &QueryRequest) -> Vec<ResultItem> {
        let query = request.text.trim();
        if query.is_empty() {
            return Vec::new();
        }

        let url = build_url(&self.engine.template(), query);
        let item = row(query, &url);
        self.routes.remember(&item.id, &url);
        vec![item]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(provider: &str, title: &str) -> ResultItem {
        ResultItem::new(provider, format!("{provider}:x"), title)
    }

    /// A throw-away directory, unique to this test process and one name.
    fn temp_dir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lumen-websearch-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("create a temp dir");
        dir
    }

    // ---- the id ----

    #[test]
    fn the_id_is_the_sha1_of_the_url() {
        assert_eq!(sha1_hex(""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex("The quick brown fox jumps over the lazy dog"),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn sha1_pads_the_tail_correctly_at_every_block_boundary() {
        // 55 bytes is the last length whose terminator fits in one block, 56 the first that needs
        // a second; the third is the two-block example from RFC 3174.
        let one_block = "a".repeat(55);
        assert_eq!(
            sha1_hex(&one_block),
            "c1c8bbdc22796e28c0e15163d20899b65621d65a"
        );
        let two_blocks = "a".repeat(56);
        assert_eq!(
            sha1_hex(&two_blocks),
            "c2db330f6083854c99d4b5bfb6e8f29f201be699"
        );
        assert_eq!(
            sha1_hex("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    // ---- the url ----

    #[test]
    fn the_query_is_percent_encoded() {
        assert_eq!(percent_encode("hello"), "hello");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e", "unreserved");
        assert_eq!(percent_encode("2 + 2"), "2%20%2B%202");
        assert_eq!(percent_encode("a&b=c?"), "a%26b%3Dc%3F");
        assert_eq!(percent_encode("café"), "caf%C3%A9", "utf-8, byte by byte");
        assert_eq!(percent_encode("100%"), "100%25");
    }

    #[test]
    fn the_query_replaces_the_placeholder_in_the_configured_engine() {
        assert_eq!(
            build_url(DEFAULT_ENGINE, "hello world"),
            "https://duckduckgo.com/?q=hello%20world"
        );
        assert_eq!(
            build_url("https://searx.example/search?q=%s&lang=en", "10 km in miles"),
            "https://searx.example/search?q=10%20km%20in%20miles&lang=en"
        );
        assert_eq!(
            build_url("https://example.org/?q=%s&p=%s", "x"),
            "https://example.org/?q=x&p=%s",
            "only the first placeholder is the query"
        );
        assert_eq!(
            build_url(DEFAULT_ENGINE, "  padded  "),
            "https://duckduckgo.com/?q=padded",
            "the url and the title agree on the trimmed query"
        );
    }

    #[test]
    fn the_template_falls_back_to_the_contract_default() {
        assert_eq!(template_or_default(None), DEFAULT_ENGINE);
        assert_eq!(template_or_default(Some("")), DEFAULT_ENGINE);
        assert_eq!(template_or_default(Some("   ")), DEFAULT_ENGINE);
        assert_eq!(
            template_or_default(Some("https://example.org/nothing")),
            DEFAULT_ENGINE,
            "a template without %s cannot carry the query"
        );
        assert_eq!(
            template_or_default(Some("  https://example.org/?q=%s  ")),
            "https://example.org/?q=%s"
        );
    }

    // ---- the engine setting ----

    #[test]
    fn the_engine_setting_reads_the_key_through_gsettings() {
        let argv = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&argv);
        let setting = EngineSetting::with_runner(
            Some(PathBuf::from("/tmp/lumen-schemas")),
            Arc::new(move |program: &str, args: &[String]| {
                let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
                seen.push(program.to_string());
                seen.extend(args.iter().cloned());
                Some("'https://searx.example/search?q=%s'\n".to_string())
            }),
        );

        assert_eq!(
            setting.schema_value().as_deref(),
            Some("https://searx.example/search?q=%s")
        );
        assert_eq!(
            setting.template(),
            "https://searx.example/search?q=%s",
            "the schema's value is the engine the row is built with"
        );

        let seen = argv.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(
            seen[0], "gsettings",
            "the daemon reads GSettings through its CLI"
        );
        assert_eq!(seen[1], "--schemadir");
        assert_eq!(seen[2], "/tmp/lumen-schemas");
        assert_eq!(seen[3], "get");
        assert_eq!(seen[4], SCHEMA_ID);
        assert_eq!(seen[5], ENGINE_KEY);
    }

    #[test]
    fn the_engine_setting_falls_back_when_gsettings_cannot_answer() {
        let unavailable = EngineSetting::with_runner(None, Arc::new(|_, _| None));
        assert_eq!(unavailable.schema_value(), None);
        assert_eq!(
            unavailable.template(),
            DEFAULT_ENGINE,
            "a machine without the schema is not an error (contract §9)"
        );

        let nonsense =
            EngineSetting::with_runner(None, Arc::new(|_, _| Some("not a gvariant\n".into())));
        assert_eq!(nonsense.template(), DEFAULT_ENGINE, "unparseable output");

        let empty = EngineSetting::with_runner(None, Arc::new(|_, _| Some("''\n".into())));
        assert_eq!(empty.template(), DEFAULT_ENGINE, "an empty template");
    }

    #[test]
    fn a_gvariant_string_is_unquoted_and_unescaped() {
        assert_eq!(
            parse_gvariant_string("'https://x/?q=%s'\n").as_deref(),
            Some("https://x/?q=%s")
        );
        assert_eq!(parse_gvariant_string("  'a\\'b'  ").as_deref(), Some("a'b"));
        assert_eq!(parse_gvariant_string("'a\\\\b'").as_deref(), Some("a\\b"));
        assert_eq!(parse_gvariant_string("https://x").as_deref(), None);
        assert_eq!(parse_gvariant_string("").as_deref(), None);
    }

    #[test]
    fn the_real_schema_is_read_through_gsettings() {
        // The shipped schema is the fixture: compile a copy whose `search-engine` default is not
        // the contract one, so a hard-coded template could not pass.
        let Some(dir) = compiled_schema_dir("https://searx.example/search?q=%s") else {
            eprintln!("skipped: glib-compile-schemas is not installed");
            return;
        };
        assert_eq!(
            EngineSetting::reading_schema(dir).template(),
            "https://searx.example/search?q=%s",
            "the engine comes from the shared schema, not from a constant"
        );

        let absent = EngineSetting::reading_schema(temp_dir("no-schemas"));
        assert_eq!(
            absent.template(),
            DEFAULT_ENGINE,
            "no schema installed is the documented fallback, never a failure"
        );
    }

    /// Compile the repository's schema with a different `search-engine` default into a throw-away
    /// directory. `None` when `glib-compile-schemas` is missing, so the test can skip.
    fn compiled_schema_dir(engine: &str) -> Option<PathBuf> {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../extension/schemas/org.gnome.shell.extensions.lumen.gschema.xml");
        let xml = std::fs::read_to_string(&source).ok()?;
        let xml = xml.replace(
            "<default>'https://duckduckgo.com/?q=%s'</default>",
            &format!("<default>'{engine}'</default>"),
        );
        let dir = temp_dir("schema");
        std::fs::write(dir.join("org.gnome.shell.extensions.lumen.gschema.xml"), xml).ok()?;

        let compiled = std::process::Command::new("glib-compile-schemas")
            .arg(&dir)
            .status()
            .ok()?;
        if !compiled.success() {
            return None;
        }
        Some(dir)
    }

    // ---- the row ----

    #[test]
    fn the_row_is_the_contract_shape() {
        let url = build_url(DEFAULT_ENGINE, "hello world");
        let item = row("hello world", &url);

        assert_eq!(item.provider, PROVIDER);
        assert_eq!(
            item.id, "web:ec78c95bb90880898dc506e4af92696db720cdcf",
            "the id is the sha1 of the url, as contract §3 spells it"
        );
        assert_eq!(item.title, "Search the web for hello world");
        assert_eq!(item.subtitle, "");
        assert_eq!(item.icon, "", "no themed icon: the shell falls back");
        assert_eq!(item.score, 0.0, "the registry scores every row");
        assert_eq!(
            item.actions,
            vec![Action::new("open", "Search", "shell")],
            "opening it is a shell action (contract §2)"
        );
        assert!(item.preview.is_empty(), "contract §6: an empty payload");

        let wired = item.row();
        assert_eq!(
            zbus::DynamicType::signature(&wired).to_string(),
            "a{sv}",
            "and the row survives the wire"
        );
        assert_eq!(
            crate::protocol::string_field(&wired, "id").as_deref(),
            Some("web:ec78c95bb90880898dc506e4af92696db720cdcf")
        );
    }

    #[test]
    fn an_empty_query_gets_no_row() {
        let search = WebSearch::new(EngineSetting::with_runner(None, Arc::new(|_, _| None)));
        for text in ["", "   ", "\t"] {
            assert!(
                search
                    .query(&QueryRequest::new(text, Vec::new()))
                    .is_empty(),
                "`{text}` is not something to search the web for"
            );
        }
    }

    #[test]
    fn the_provider_builds_the_row_and_remembers_its_url() {
        let search = WebSearch::new(EngineSetting::with_runner(
            None,
            Arc::new(|_, _| Some("'https://searx.example/?q=%s'".to_string())),
        ));
        assert_eq!(search.id(), PROVIDER);
        assert!(search.available(), "the fallback needs no data source");

        let results = search.query(&QueryRequest::new("  hello world ", Vec::new()));
        assert_eq!(results.len(), 1);
        let url = "https://searx.example/?q=hello%20world";
        assert_eq!(results[0].id, format!("web:{}", sha1_hex(url)));
        assert_eq!(results[0].title, "Search the web for hello world");
        assert_eq!(
            search.routes().url_of(&results[0].id).as_deref(),
            Some(url),
            "the id is one-way, so the url has to be remembered"
        );
    }

    // ---- the threshold ----

    #[test]
    fn a_word_start_match_is_strong_but_a_substring_is_not() {
        let query = "term";
        for (provider, title) in [
            ("applications", "Terminal"),
            ("applications", "GNOME Terminal"),
            ("files", "Terminal notes"),
        ] {
            assert!(
                has_strong_match(&[item(provider, title)], query),
                "`{title}` (from {provider}) matches `{query}` at a word start"
            );
        }
        assert!(
            !has_strong_match(&[item("files", "xterm-settings.txt")], query),
            "a substring is a weak match: the row is still offered"
        );
        assert!(
            !has_strong_match(&[item("applications", "Trash Emptier")], query),
            "a subsequence is a weak match"
        );
        assert!(!has_strong_match(&[], query));
    }

    #[test]
    fn the_threshold_is_the_word_start_quality() {
        assert!(
            (ranking::match_quality("GNOME Terminal", "term") - STRONG_MATCH_QUALITY).abs() < 1e-9,
            "the threshold is the weakest match that still reads as `you meant this`"
        );
        assert!(
            ranking::match_quality("Terminal", "term") > STRONG_MATCH_QUALITY,
            "a prefix match is stronger than the threshold"
        );
        assert!(
            ranking::match_quality("xterm-settings.txt", "term") < STRONG_MATCH_QUALITY,
            "a substring match is weaker than the threshold"
        );
    }

    #[test]
    fn a_recognised_expression_counts_as_a_strong_match() {
        assert!(
            has_strong_match(&[item("calculator", "4")], "2 + 2"),
            "the calculator only answers when the query parses (contract §5)"
        );
    }

    #[test]
    fn should_offer_needs_a_query_and_no_strong_match() {
        assert!(should_offer(&[], "hello"));
        assert!(!should_offer(&[], "   "), "nothing to search the web for");
        assert!(!should_offer(&[item("applications", "Files")], "files"));
        assert!(
            should_offer(&[item(PROVIDER, "Search the web for hello")], "hello"),
            "the row is not a match itself, so a dead-end query stays offered"
        );
    }

    // ---- the routes ----

    #[test]
    fn the_routes_remember_what_they_offered() {
        let routes = Routes::new();
        assert_eq!(routes.url_of("web:abc"), None);
        routes.remember("web:abc", "https://example.org/?q=a");
        assert_eq!(
            routes.url_of("web:abc").as_deref(),
            Some("https://example.org/?q=a")
        );
        assert_eq!(routes.url_of("web:def"), None);
    }

    #[test]
    fn the_routes_are_bounded() {
        let routes = Routes::new();
        for index in 0..(MAX_ROUTES + 10) {
            routes.remember(&format!("web:{index}"), "https://example.org/?q=x");
        }
        let remembered = (0..(MAX_ROUTES + 10))
            .filter(|index| routes.url_of(&format!("web:{index}")).is_some())
            .count();
        assert!(
            remembered <= MAX_ROUTES,
            "a long session must not grow the table without bound, got {remembered}"
        );
        assert!(
            routes.url_of(&format!("web:{}", MAX_ROUTES + 9)).is_some(),
            "the newest row is always routable"
        );
    }
}
