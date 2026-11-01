//! GNOME LocalSearch / TinySPARQL, reached by `dlopen`.
//!
//! The daemon links no part of libtinysparql: the library has no stable development headers to
//! build against, so its C ABI is declared here and resolved at run time. A machine without the
//! library, or without an endpoint answering on the bus, simply has no `files` capability
//! (contract §9) — nothing else in the daemon changes.
//!
//! The API used is the supported one: `tracker_sparql_connection_bus_new()` points a Sparql
//! connection at the `org.freedesktop.LocalSearch3` endpoint, `tracker_sparql_connection_query()`
//! runs the query the pure layer in [`crate::excludes`] builds, and the cursor accessors read the
//! rows back. Every query runs on the search thread, never on the D-Bus dispatch thread.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::Mutex;

use crate::excludes::{self, FileQuery};
use crate::index::{uri_to_path, FileHit, FileIndex};

/// The endpoint bus names, in the order they are tried: the current name first, then the
/// compatibility name LocalSearch installs alongside it.
const ENDPOINTS: [&str; 2] = [
    "org.freedesktop.LocalSearch3",
    "org.freedesktop.Tracker3.Miner.Files",
];

/// Library sonames, in the order they are tried. `LUMEN_TINYSPARQL_LIB` overrides the search
/// with one path, which is also how a test proves the "library missing" path.
const SONAMES: [&str; 3] = [
    "libtinysparql-3.0.so.0",
    "libtinysparql-3.0.so",
    "libtracker-sparql-3.0.so.0",
];

/// `GError` is opaque here; only its message is ever read.
#[repr(C)]
struct GError {
    domain: u32,
    code: c_int,
    message: *mut c_char,
}

type Obj = *mut c_void;
type ErrorPtr = *mut GError;
/// `GError **`, the out-parameter every fallible call takes.
type ErrorOut = *mut ErrorPtr;

type ConnectionBusNew = unsafe extern "C" fn(*const c_char, *const c_char, Obj, ErrorOut) -> Obj;
type ConnectionQuery = unsafe extern "C" fn(Obj, *const c_char, Obj, ErrorOut) -> Obj;
type CursorNext = unsafe extern "C" fn(Obj, Obj, ErrorOut) -> c_int;
type CursorGetString = unsafe extern "C" fn(Obj, c_int, *mut c_uint) -> *const c_char;
type CursorGetInteger = unsafe extern "C" fn(Obj, c_int) -> i64;
type CursorIsBound = unsafe extern "C" fn(Obj, c_int) -> c_int;
type CursorGetNColumns = unsafe extern "C" fn(Obj) -> c_int;
type CursorGetVariableName = unsafe extern "C" fn(Obj, c_int) -> *const c_char;
type CursorClose = unsafe extern "C" fn(Obj);
type ObjectUnref = unsafe extern "C" fn(Obj);
type ErrorFree = unsafe extern "C" fn(ErrorPtr);

/// The functions of libtinysparql this daemon uses, resolved once.
struct Symbols {
    connection_bus_new: ConnectionBusNew,
    connection_query: ConnectionQuery,
    cursor_next: CursorNext,
    cursor_get_string: CursorGetString,
    cursor_get_integer: CursorGetInteger,
    cursor_is_bound: CursorIsBound,
    cursor_get_n_columns: CursorGetNColumns,
    cursor_get_variable_name: CursorGetVariableName,
    cursor_close: CursorClose,
    object_unref: ObjectUnref,
    error_free: ErrorFree,
}

/// A library kept open for the process's lifetime. The symbols inside stay valid with it.
struct Library {
    /// Kept open for the process's lifetime, so the resolved symbols stay valid. Never closed:
    /// the daemon has one index and no reason to unload it.
    #[allow(dead_code)]
    handle: *mut c_void,
    symbols: Symbols,
}

// The library and the GObject connection it hands out live for as long as the index does; access
// to the connection is serialised by the index's mutex.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// Load the library and resolve every symbol needed below.
    fn load() -> Result<Self, String> {
        let candidates: Vec<String> = match std::env::var("LUMEN_TINYSPARQL_LIB") {
            Ok(path) if !path.is_empty() => vec![path],
            _ => SONAMES.iter().map(|name| (*name).to_string()).collect(),
        };

        let mut failures = Vec::new();
        for candidate in candidates {
            match Self::load_from(&candidate) {
                Ok(library) => return Ok(library),
                Err(reason) => failures.push(reason),
            }
        }
        Err(format!("cannot load libtinysparql ({})", failures.join("; ")))
    }

    fn load_from(path: &str) -> Result<Self, String> {
        let name = CString::new(path).map_err(|e| format!("{path}: {e}"))?;
        // RTLD_NOW: a missing symbol is a load failure, not a crash at first query.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!("{path}: {}", dl_error()));
        }

        // glib and gobject are dependencies of the library, so this handle resolves them too; the
        // explicit fallback covers a libtinysparql built without them in its search scope.
        let glib = if unsafe { libc::dlsym(handle, c"g_object_unref".as_ptr()) }.is_null() {
            let fallback = CString::new("libgobject-2.0.so.0").expect("a literal soname");
            let fallback = unsafe { libc::dlopen(fallback.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if fallback.is_null() {
                return Err(format!("{path}: g_object_unref is missing"));
            }
            fallback
        } else {
            handle
        };

        let symbols = SymbolTable { primary: handle, fallback: glib };
        let library = Self {
            handle,
            symbols: Symbols {
                connection_bus_new: symbols.resolve("tracker_sparql_connection_bus_new")?,
                connection_query: symbols.resolve("tracker_sparql_connection_query")?,
                cursor_next: symbols.resolve("tracker_sparql_cursor_next")?,
                cursor_get_string: symbols.resolve("tracker_sparql_cursor_get_string")?,
                cursor_get_integer: symbols.resolve("tracker_sparql_cursor_get_integer")?,
                cursor_is_bound: symbols.resolve("tracker_sparql_cursor_is_bound")?,
                cursor_get_n_columns: symbols.resolve("tracker_sparql_cursor_get_n_columns")?,
                cursor_get_variable_name: symbols.resolve("tracker_sparql_cursor_get_variable_name")?,
                cursor_close: symbols.resolve("tracker_sparql_cursor_close")?,
                object_unref: symbols.resolve::<ObjectUnref>("g_object_unref")?,
                error_free: symbols.resolve::<ErrorFree>("g_error_free")?,
            },
        };
        Ok(library)
    }
}

/// The two handles a symbol may come from: the library itself, and the GObject runtime.
struct SymbolTable {
    primary: *mut c_void,
    fallback: *mut c_void,
}

impl SymbolTable {
    fn raw(&self, name: &str, handle: *mut c_void) -> Result<*mut c_void, String> {
        let name = CString::new(name).map_err(|e| format!("{e}"))?;
        let symbol = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if symbol.is_null() {
            return Err(format!("{name:?} is missing: {}", dl_error()));
        }
        Ok(symbol)
    }

    fn primary_or_fallback(&self, name: &str) -> Result<*mut c_void, String> {
        self.raw(name, self.primary)
            .or_else(|_| self.raw(name, self.fallback))
    }

    fn resolve<T: Copy>(&self, name: &str) -> Result<T, String> {
        let symbol = self.primary_or_fallback(name)?;
        if std::mem::size_of::<T>() != std::mem::size_of::<*mut c_void>() {
            return Err(format!("{name:?} does not fit a function pointer"));
        }
        Ok(unsafe { std::mem::transmute_copy(&symbol) })
    }
}

fn dl_error() -> String {
    let error = unsafe { libc::dlerror() };
    if error.is_null() {
        return "no detail".to_string();
    }
    unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
}

fn take_error(error: ErrorPtr, free: ErrorFree) -> String {
    if error.is_null() {
        return "no detail".to_string();
    }
    let message = unsafe { (*error).message };
    let text = if message.is_null() {
        "no detail".to_string()
    } else {
        unsafe { CStr::from_ptr(message) }.to_string_lossy().into_owned()
    };
    unsafe { free(error) };
    text
}

/// The open connection, kept for the process's lifetime.
struct Endpoint {
    connection: Obj,
    /// Which bus name answered, for the log line.
    name: &'static str,
}

unsafe impl Send for Endpoint {}
unsafe impl Sync for Endpoint {}

/// File and folder search against the system's LocalSearch index.
pub struct LocalSearchIndex {
    library: Library,
    endpoint: Endpoint,
    /// One query at a time: the connection is a plain GObject and the caller is the search thread.
    query_lock: Mutex<()>,
}

impl LocalSearchIndex {
    /// Load the library and prove the endpoint answers before claiming the capability.
    pub fn open() -> Result<Self, String> {
        let library = Library::load()?;
        let endpoint = connect(&library)?;
        Ok(Self {
            library,
            endpoint,
            query_lock: Mutex::new(()),
        })
    }

    /// The bus name the index answered on.
    pub fn endpoint_name(&self) -> &'static str {
        self.endpoint.name
    }

    /// Run a query and read the rows back. The column order is taken from the cursor's own
    /// variable names, so a projection change cannot silently shift a value.
    fn run(&self, sparql: &str) -> Result<Vec<FileHit>, String> {
        let symbols = &self.library.symbols;
        let query = CString::new(sparql).map_err(|e| format!("the query has a NUL byte: {e}"))?;

        let mut error: ErrorPtr = std::ptr::null_mut();
        let cursor = unsafe {
            (symbols.connection_query)(
                self.endpoint.connection,
                query.as_ptr(),
                std::ptr::null_mut(),
                &mut error,
            )
        };
        if cursor.is_null() {
            return Err(take_error(error, symbols.error_free));
        }

        let result = self.read_rows(cursor);
        unsafe {
            (symbols.cursor_close)(cursor);
            (symbols.object_unref)(cursor);
        }
        result
    }

    fn read_rows(&self, cursor: Obj) -> Result<Vec<FileHit>, String> {
        let symbols = &self.library.symbols;
        let columns = unsafe { (symbols.cursor_get_n_columns)(cursor) };
        if columns < 0 {
            return Err("the cursor reported no columns".to_string());
        }

        let mut by_name: HashMap<String, c_int> = HashMap::new();
        for index in 0..columns {
            let name = unsafe { (symbols.cursor_get_variable_name)(cursor, index) };
            if name.is_null() {
                continue;
            }
            let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
            by_name.insert(name, index);
        }
        let column = |name: &str| -> Result<c_int, String> {
            by_name
                .get(name)
                .copied()
                .ok_or_else(|| format!("the cursor has no `{name}` column"))
        };
        let file_column = column("file")?;
        let name_column = column("name")?;
        let size_column = by_name.get("size").copied();
        let mtime_column = by_name.get("mtime").copied();

        let mut hits = Vec::new();
        loop {
            let mut error: ErrorPtr = std::ptr::null_mut();
            let more = unsafe {
                (symbols.cursor_next)(cursor, std::ptr::null_mut(), &mut error)
            };
            if more == 0 {
                if error.is_null() {
                    break;
                }
                // Keep the rows already read; a truncated page is better than none, and the
                // caller can still ask for the next one.
                eprintln!(
                    "lumen-daemon: the file query stopped early: {}",
                    take_error(error, symbols.error_free)
                );
                break;
            }

            let Some(uri) = self.string_at(cursor, file_column)? else {
                continue;
            };
            let Some(path) = uri_to_path(&uri) else {
                continue;
            };
            let mut hit = FileHit::new(path);
            if let Some(name) = self.string_at(cursor, name_column)? {
                hit.name = name;
            }
            if let Some(column) = size_column {
                if self.is_bound(cursor, column) {
                    hit.size = unsafe { (symbols.cursor_get_integer)(cursor, column) }.max(0) as u64;
                }
            }
            if let Some(column) = mtime_column {
                if let Some(stamp) = self.string_at(cursor, column)? {
                    hit.mtime_ms = crate::index::parse_iso8601_ms(&stamp).unwrap_or(0);
                }
            }
            // LocalSearch models folders and files as the same `nfo:FileDataObject` type, so the
            // filesystem is what says which one this is.
            hit.is_folder = std::fs::metadata(&hit.path)
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            hits.push(hit);
        }

        Ok(hits)
    }

    fn is_bound(&self, cursor: Obj, column: c_int) -> bool {
        let bound = unsafe { (self.library.symbols.cursor_is_bound)(cursor, column) };
        bound != 0
    }

    fn string_at(&self, cursor: Obj, column: c_int) -> Result<Option<String>, String> {
        if !self.is_bound(cursor, column) {
            return Ok(None);
        }
        let raw = unsafe {
            (self.library.symbols.cursor_get_string)(cursor, column, std::ptr::null_mut())
        };
        if raw.is_null() {
            return Ok(None);
        }
        Ok(Some(
            unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned(),
        ))
    }
}

impl FileIndex for LocalSearchIndex {
    fn search(&self, query: &str, limit: usize, offset: usize) -> Vec<FileHit> {
        let request = FileQuery::new(query, limit, offset);
        if request.is_empty() || limit == 0 {
            return Vec::new();
        }

        let _one_at_a_time = self.query_lock.lock().unwrap_or_else(|e| e.into_inner());

        // The same exclusion rules the in-process filter applies go into the query too, so noise
        // does not consume the page.
        let hits = match self.run(&request.sparql()) {
            Ok(hits) => hits,
            Err(reason) => {
                eprintln!(
                    "lumen-daemon: the file query was rejected ({reason}); asking by name only"
                );
                match self.run(&request.name_only_sparql()) {
                    Ok(hits) => hits,
                    Err(reason) => {
                        eprintln!("lumen-daemon: the file query failed: {reason}");
                        Vec::new()
                    }
                }
            }
        };

        // The query's FILTER is a hint the endpoint may implement more weakly than we do (a URI
        // ending in `/target` is not caught by `fn:contains`); this is the rule that holds.
        hits.into_iter()
            .filter(|hit| !excludes::is_excluded(&hit.path))
            .collect()
    }

    fn available(&self) -> bool {
        true
    }
}

/// Open a connection to the first endpoint that answers, and prove it answers.
fn connect(library: &Library) -> Result<Endpoint, String> {
    let symbols = &library.symbols;
    let mut failures = Vec::new();
    for name in ENDPOINTS {
        let Ok(bus_name) = CString::new(name) else {
            continue;
        };
        let mut error: ErrorPtr = std::ptr::null_mut();
        let connection = unsafe {
            (symbols.connection_bus_new)(
                bus_name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut error,
            )
        };
        if connection.is_null() {
            failures.push(format!("{name}: {}", take_error(error, symbols.error_free)));
            continue;
        }

        // A GDBus proxy is lazy: creating one succeeds even when nothing is listening, so the
        // probe query is what decides whether the endpoint is really there.
        let probe = CString::new(FileQuery::probe_sparql()).expect("a literal query");
        let mut error: ErrorPtr = std::ptr::null_mut();
        let cursor = unsafe {
            (symbols.connection_query)(
                connection,
                probe.as_ptr(),
                std::ptr::null_mut(),
                &mut error,
            )
        };
        if cursor.is_null() {
            let reason = take_error(error, symbols.error_free);
            unsafe { (symbols.object_unref)(connection) };
            failures.push(format!("{name}: {reason}"));
            continue;
        }
        // Reading the first row is enough: an empty index still proves the endpoint answers.
        let mut error: ErrorPtr = std::ptr::null_mut();
        unsafe {
            (symbols.cursor_next)(cursor, std::ptr::null_mut(), &mut error);
            (symbols.cursor_close)(cursor);
            (symbols.object_unref)(cursor);
        }
        if !error.is_null() {
            let reason = take_error(error, symbols.error_free);
            unsafe { (symbols.object_unref)(connection) };
            failures.push(format!("{name}: {reason}"));
            continue;
        }

        return Ok(Endpoint {
            connection,
            name,
        });
    }

    Err(format!("no search endpoint answered ({})", failures.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_is_reported_as_unavailable() {
        // The override is the documented test seam: pointing it at a path with no library must
        // fail cleanly rather than panic or hang.
        let error = match Library::load_from("/nonexistent/libtinysparql-3.0.so.0") {
            Err(error) => error,
            Ok(_) => panic!("there is no library at that path"),
        };
        assert!(error.contains("libtinysparql"), "{error}");
    }

    #[test]
    fn symbols_are_resolved_by_name_and_missing_ones_are_reported() {
        let table = SymbolTable {
            primary: unsafe { libc::dlopen(c"libc.so.6".as_ptr(), libc::RTLD_NOW) },
            fallback: std::ptr::null_mut(),
        };
        assert!(table.resolve::<Obj>("getpid").is_ok());
        let missing = table.raw("lumen_symbol_that_does_not_exist", table.primary);
        assert!(missing.unwrap_err().contains("missing"));
    }

    /// The real endpoint, on a machine that has one: `cargo test -- --ignored`.
    ///
    /// It runs against the *session* bus, so it only says anything when LocalSearch is installed
    /// and has indexed files (see the ticket's handoff notes for the recorded output).
    #[test]
    #[ignore = "needs a live LocalSearch endpoint on the session bus"]
    fn the_live_endpoint_answers_a_name_query() {
        let index = LocalSearchIndex::open().expect("a live LocalSearch endpoint");
        assert_eq!(index.endpoint_name(), "org.freedesktop.LocalSearch3");
        let hits = index.search("readme", 5, 0);
        assert!(!hits.is_empty(), "the index should know some README files");
        for hit in &hits {
            assert!(hit.path.is_absolute(), "{:?}", hit.path);
            assert!(!excludes::is_excluded(&hit.path), "{:?}", hit.path);
            assert!(
                hit.name.to_lowercase().contains("readme"),
                "{} does not match the query",
                hit.name
            );
        }
    }
}
