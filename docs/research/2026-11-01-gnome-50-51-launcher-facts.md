# GNOME 50/51 launcher: primary-source facts (research note, 2026-11-01)

Scope: two-process GNOME Wayland launcher (GJS shell extension + Rust daemon over IPC).
Every claim below is sourced; uncertainty is flagged inline with **[UNVERIFIED]**. No product
recommendation is made.

Reference points (all primary):

- GNOME 50 "Tokyo" released **2026-03-18** — https://release.gnome.org/50/
- GNOME 51 "A Coruña" released **2026-09-16** — https://release.gnome.org/51/ (current stable)
- GNOME 48 "Bengaluru" released **2025-03-19** — https://release.gnome.org/48/
- Distro versions observed while checking file layouts (Arch `extra`, 2026-10/11):
  gnome-shell 1:51.1, mutter (unversioned check), gnome-control-center 51.1,
  xdg-desktop-portal-gnome 51.0, nautilus 51.1, **localsearch 3.12.0**, **tinysparql 3.12.0**.
- systemd docs referenced are **systemd 262** (latest at time of writing).

---

## 1. Querying GNOME LocalSearch / TinySPARQL

### 1.1 D-Bus surface (exact names, paths, signatures)

Intended entry point is the **GObject library**, not raw D-Bus:

> "The LocalSearch SPARQL endpoint can be contacted at the `org.freedesktop.LocalSearch3` D-Bus
> name, **use the TinySPARQL library to set up a connection to it**."
> — https://gnome.pages.gitlab.gnome.org/localsearch/endpoint.html

| Thing | Value | Source |
|---|---|---|
| Data endpoint bus name | `org.freedesktop.LocalSearch3` | localsearch `endpoint.html` |
| Legacy/compat bus name (same endpoint) | `org.freedesktop.Tracker3.Miner.Files` | https://blogs.gnome.org/carlosg/2024/07/14/goodbye-tracker-hello-tinysparql-and-localsearch/ ; both `.service` files installed (see §4.1) |
| Object path | `/org/freedesktop/Tracker3/Endpoint` (default) | `tracker_endpoint_dbus_new()` docs + source, https://gnome.pages.gitlab.gnome.org/tinysparql/class.EndpointDBus.html |
| Interface | `org.freedesktop.Tracker3.Endpoint` (**not** renamed to LocalSearch) | source |
| Control service | `org.freedesktop.LocalSearch3.Control`, ifaces `org.freedesktop.Tracker3.Miner.Files.Index` (`IndexLocation(s, as, as)`) and `org.freedesktop.Tracker3.Miner.Files.Proxy` (`Graphs(as)`, `IndexedLocations(as)`) | https://gnome.pages.gitlab.gnome.org/localsearch/dbus-api.html |
| Writeback service | `org.freedesktop.LocalSearch3.Writeback`, method `Writeback(IN a{sv} rdf)` | same page |

The introspection XML is a literal string in the endpoint implementation
(`src/libtinysparql/tracker-endpoint-dbus.c`, current path `src/libtinysparql/` after the
`libtracker-sparql` → `libtinysparql` rename):

```xml
<node>
  <interface name='org.freedesktop.Tracker3.Endpoint'>
    <method name='Query'>
      <arg type='s' name='query' direction='in' />
      <arg type='h' name='output_stream' direction='in' />
      <arg type='a{sv}' name='arguments' direction='in' />
      <arg type='as' name='result' direction='out' />
    </method>
    <method name='Serialize'>
      <arg type='s' name='query' direction='in' />
      <arg type='h' name='output_stream' direction='in' />
      <arg type='i' name='flags' direction='in' />
      <arg type='i' name='format' direction='in' />
      <arg type='a{sv}' name='arguments' direction='in' />
    </method>
    <method name='Update'>        <arg type='h' name='input_stream' direction='in' /></method>
    <method name='UpdateArray'>   <arg type='h' name='input_stream' direction='in' /></method>
    <method name='UpdateBlank'>   <arg type='h' name='input_stream' direction='in' />
                                   <arg type='aaa{ss}' name='result' direction='out' /></method>
    <method name='Deserialize'>
      <arg type='h' name='input_stream' direction='in' />
      <arg type='i' name='flags' direction='in' />
      <arg type='i' name='format' direction='in' />
      <arg type='s' name='default_graph' direction='in' />
      <arg type='a{sv}' name='arguments' direction='in' />
    </method>
    <signal name='GraphUpdated'><arg type='sa{ii}' name='updates' /></signal>
  </interface>
</node>
```
Source: https://raw.githubusercontent.com/GNOME/tinysparql/main/src/libtinysparql/tracker-endpoint-dbus.c

### 1.2 How results are marshalled — yes, it is a private FD/byte-stream protocol

- Query arguments are real D-Bus types; **the result payload is not**. `Query` takes a D-Bus
  UNIX FD (`h`) that the *caller* creates (a pipe), and the server writes rows into it with a
  `GDataOutputStream` in **host byte order** (`g_data_output_stream_set_byte_order(...,
  G_DATA_STREAM_BYTE_ORDER_HOST_ENDIAN)`).
- Row layout as written by `write_cursor()`:
  1. `int32` number of columns (repeated per row);
  2. per column: `int32` value type (the `TrackerSparqlValueType` enum — enum values in
     `tracker-enums.h`, **[UNVERIFIED] I did not read that header**);
  3. per column: `int32` cumulative byte offset into the following string blob;
  4. per column: NUL-terminated UTF-8 value; for a language-tagged string, `value\0langtag\0`.
- The asynchronous return value is `as result` = the variable names.
- `Update`/`UpdateArray`/`UpdateBlank` take an input FD carrying a host-endian
  `GDataStream`: `uint32` op type (`TRACKER_BUS_OP_SPARQL=0`, `_RDF=1`, `_DBUS_FD=2` — see
  `src/libtinysparql/bus/tracker-bus.h`), then `int32` length + bytes, then a trailing `uint32 0`
  (asserted in `handle_read_update()`).
- Practical consequence for both GJS and Rust: you must (a) create a pipe, (b) pass the write end
  as a D-Bus FD, (c) read and parse the byte stream yourself, (d) reimplement lock-step parsing for
  every TinySPARQL release. **There is no public specification of this stream**; the only
  definitions are the C source and the private header above.
  - **In GJS**: possible but manual — `GLib.unix_open_pipe()` / `Gio.UnixOutputStream`,
    `Gio.UnixFDList`, then read into a `ByteArray`/`ArrayBuffer` and parse with `DataView`. The
    shell's main loop cannot block, so it must be fully async. This is not a small amount of code and
    is coupled to the C implementation.
  - **In Rust/zbus**: `zvariant` can carry FDs (`zvariant::OwnedFd` / `Fd`) and `zbus` can send
    them; you still hand-write the stream codec. Nothing in zbus knows this format.
- The interface *is* registered with introspection data
  (`g_dbus_connection_register_object(..., node_info->interfaces[0], ...)`), so `gdbus introspect`
  / `busctl introspect` will list the methods — but that does **not** make it callable by generic
  tools, because you must supply the FD. **[UNVERIFIED]** (not run against a live 50/51 machine).

### 1.3 GObject-Introspection typelib (the supported path)

- Installed files (Arch `tinysparql` 3.12.0, https://archlinux.org/packages/extra/x86_64/tinysparql/files/):
  - `/usr/lib/girepository-1.0/Tsparql-3.0.typelib`
  - `/usr/lib/girepository-1.0/Tracker-3.0.typelib`
  - `/usr/share/gir-1.0/{Tsparql-3.0.gir, Tracker-3.0.gir}`
  - `/usr/include/tinysparql-3.0/…`, `/usr/lib/pkgconfig/tinysparql-3.0.pc`,
    `/usr/lib/pkgconfig/tracker-sparql-3.0.pc`
- So in GJS: `import Tsparql from 'gi://Tsparql?version=3.0';` (namespace `Tsparql`), or the older
  namespace `gi://Tracker?version=3.0` (both typelibs ship). Docs use the Python equivalent
  (`gi.require_version('Tsparql', '3.0')`) — https://gnome.pages.gitlab.gnome.org/localsearch/endpoint.html
- Usage:
  ```js
  const conn = Tsparql.SparqlConnection.bus_new('org.freedesktop.LocalSearch3', null);
  ```
  The sync `bus_new()` blocks; `bus_new_async()` exists (see
  https://gnome.pages.gitlab.gnome.org/tinysparql/ctor.SparqlConnection.bus_new.html and
  `class.SparqlConnection.html`).
- Presence on default GNOME installs: `nautilus` (in the Arch `gnome` group) hard-depends on both
  `localsearch` and `tinysparql` — https://archlinux.org/packages/extra/x86_64/nautilus/ ;
  the GNOME Flatpak SDK/runtime also includes TinySPARQL
  (https://gnome.pages.gitlab.gnome.org/tinysparql/sandboxing.html). It is *not* a guaranteed
  dependency of minimal/non-GNOME images — check at runtime rather than assume.

### 1.4 CLI

**Binary names are not what the LocalSearch docs say.** In 3.12.0:

- `tinysparql` package installs exactly one CLI: `/usr/bin/tinysparql` (plus helper
  `/usr/lib/tinysparql-sql`). `src/cli/meson.build` sets `main_command_name = 'tinysparql'`, and
  `src/cli/tracker-main.c` dispatches only:
  `help`, `endpoint`, `export`, `import`, `introspect`, `query`, `webide`.
  **There is no `tinysparql3` binary and no `sparql` subcommand in 3.12.**
- The LocalSearch docs example `tinysparql3 sparql -b org.freedesktop.LocalSearch3 -q "…"`
  (https://gnome.pages.gitlab.gnome.org/localsearch/endpoint.html) is **stale/incorrect for current
  versions** — the equivalent today is `tinysparql query -b org.freedesktop.LocalSearch3 "…"`.
  Flag: the CLI has been renamed/reshuffled (`tracker3` → `tinysparql`), so pin a version.
- `tinysparql query` flags (source `src/cli/tracker-query.c`, GOptionEntry table):
  `-d/--database FILE`, `-b/--dbus-service NAME`, `-r/--remote-service URI`, `-f/--file FILE`,
  `-q/--query SPARQL`, `-u/--update`, `-a/--arg NAME:TYPE:VALUE` (types `i`/`s`/`d`/`b`);
  a bare positional SPARQL string also works (argc>1 → query).
  Documented: https://gnome.pages.gitlab.gnome.org/tinysparql/commandline.html (man pages:
  `tinysparql-query.1`, `tinysparql-endpoint.1`, `tinysparql-introspect.1`, `tinysparql-export.1`,
  `tinysparql-import.1`, `tinysparql-sql.1`).
- Output format: **human text only.** `print_cursor()` prints `"Results:"` then one line per row with
  columns joined by `","`; values are stringified; empty result prints a "None"/"No results" line.
  There is no `--output-format` on `query`. Machine-readable formats exist only for
  `tinysparql export -o turtle|trig|json-ld`, `tinysparql import` (Turtle), and
  `localsearch extract|info -o turtle|trig|json-ld`.
- LocalSearch CLI: `/usr/bin/localsearch` (man pages are named `localsearch-3`, `localsearch-search`,
  `localsearch-index`, `localsearch-info`, `localsearch-status`, `localsearch-tag`,
  `localsearch-inhibit`, `localsearch-reset`, `localsearch-extract`, `localsearch-writeback-3`).
  Subcommands: `extract`, `index`, `info`, `inhibit`, `reset`, `search`, `status`, `tag`
  (`localsearch help`). `localsearch search [options] [resource-type] TERMS` with
  `-l/--limit`, `-o/--offset`, `-d/--detailed` (adds URN + snippet), `-a/--all`, `-m/--mount`,
  and type selectors `-f/--files -s/--folders -m/--music -i/--images -v/--videos
  -t/--documents --software`. Output is human text; **no JSON option** documented
  (https://gnome.pages.gitlab.gnome.org/localsearch/commandline.html). The docs statement
  "You can control LocalSearch using the `localsearch3` commandline tool" refers to the binary
  installed as `localsearch` (gettext domain is still `localsearch3`).
- **Is the CLI stable API?** No stability guarantee found in the man pages or project docs
  (**[UNVERIFIED]**: absence of evidence; I could not find an explicit "the CLI is not stable"
  statement either). Evidence of churn: the `tracker3` → `tinysparql`/`localsearch` renames, the
  docs/man-page naming inconsistencies above, and the fact that `localsearch-3(1)` says the indexer
  "is not supposed to be run by the user since it is started by systemd when the user logs in".
  Treat any shelling-out as version-pinned and throwaway.

### 1.5 Rust crates / bindings

**None exist.** crates.io search results (https://crates.io/api/v1/crates?q=<name>):

- `q=tinysparql` → 1 result, unrelated (`ohno-helper`).
- `q=tracker3` → 1 result, unrelated (`revelo-parsers-audio`).
- `q=localsearch` → 2 results, both unrelated (the `localsearch` crate is a metaheuristics library,
  https://crates.io/crates/localsearch).
- (`q=tracker-sparql` timed out at fetch time; no likely candidate exists regardless.)

There is **no wrapper for libtracker-sparql/libtinysparql and no Rust implementation of the
Tracker3/LocalSearch D-Bus protocol**. Options for a Rust daemon, in decreasing supportedness:
1. FFI to `libtinysparql-3.0` (`pkg-config tinysparql-3.0` or `tracker-sparql-3.0`; headers under
   `/usr/include/tinysparql-3.0/`) and call `tracker_sparql_connection_bus_new()`. That drags GLib/
   GObject into the process (relevant to §5).
2. Shell out to `tinysparql query` / `localsearch search` (human-text parsing).
3. Reimplement §1.2 with zbus + `zvariant::OwnedFd` (unsupported, version-coupled).

### 1.6 What the docs say a third-party app *should* do

- Flatpak/portal: "**You should not provide access within flatpaks to this D-Bus name.** Access from
  within the sandbox is handled through the TinySPARQL portal" — LocalSearch `endpoint.html`. Portal
  D-Bus activation file installed by tinysparql:
  `/usr/share/dbus-1/services/org.freedesktop.portal.Tracker.service`; user unit
  `/usr/lib/systemd/user/tinysparql-xdg-portal-3.service`; binary `/usr/lib/tinysparql-xdg-portal-3`;
  man page `tinysparql-xdg-portal-3(1)`. Access is graph-filtered per Flatpak manifest
  `--add-policy=Tracker3.dbus:<busname>:<path>=<graph>` (https://gnome.pages.gitlab.gnome.org/tinysparql/sandboxing.html).
- The GNOME-documented way for an app to surface search results in the shell is a **search provider**:
  install `$(datadir)/gnome-shell/search-providers/<id>.search-provider.ini` with keys
  `DesktopId`, `BusName`, `ObjectPath`, `Version=2`, and implement the D-Bus interface
  `org.gnome.Shell.SearchProvider2` (`GetInitialResultSet(as)→as`,
  `GetSubsearchResultSet(as,as)→as`, `GetResultMetas(as)→aa{sv}`, `ActivateResult(s,as,u)`,
  `LaunchSearch(as,u)`); the interface XML also ships as
  `$(datadir)/dbus-1/interfaces/org.gnome.ShellSearchProvider2.xml` —
  https://developer.gnome.org/documentation/tutorials/search-provider.html .
  Note this renders *inside the shell's overview*, is user-configurable in Settings → Search, and
  gives you no control over your own overlay window.
- Nothing in the LocalSearch/TinySPARQL docs forbids an ordinary unsandboxed desktop app from
  querying the endpoint; but every documented consumer uses the GObject library
  (`tinysparql`/`Tracker` typelib) rather than the raw socket protocol. **[UNVERIFIED]**: no doc
  sanctioning direct raw-D-Bus use was found.

---

## 2. Can a launcher overlay exist without a shell extension?

### 2.1 `org.freedesktop.portal.GlobalShortcuts`

- Spec: interface version **2**, portal name `org.freedesktop.portal.GlobalShortcuts`
  (`CreateSession(a{sv})→o`, `BindShortcuts(o,a(sa{sv}),s,a{sv})→o`,
  `ListShortcuts(o,a{sv})→o`, `ConfigureShortcuts(o,s,a{sv})`, signals `Activated`, `Deactivated`,
  `ShortcutsChanged`) — https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
  and the backend iface doc `doc-org.freedesktop.impl.portal.GlobalShortcuts.html`.
- **KDE implemented it first.** Plasma 5.27 (2023-02-14): "Plasma on Wayland has also gained support
  for the Global Shortcuts portal. This allows apps on Wayland to offer a standardized user interface
  for setting and editing global shortcuts." — https://kde.org/announcements/plasma/5/5.27.0/
  (also https://www.phoronix.com/news/KDE-Plasma-5.27-Released).
- **GNOME shipped it in GNOME 48** (2025-03-19): "**Global shortcuts**: apps can now create their own
  system-wide shortcuts… Apps must request permission to add global shortcuts, and global shortcuts
  can be removed from the Apps page in the Settings app." — https://release.gnome.org/48/
- GNOME's implementation is in **xdg-desktop-portal-gnome**, not mutter:
  `src/globalshortcuts.c` (present in the repo main branch and in xdg-desktop-portal-gnome 51.0,
  Arch https://archlinux.org/packages/extra/x86_64/xdg-desktop-portal-gnome/ ;
  https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/src/globalshortcuts.c). It:
  - binds a proxy to **`org.gnome.Shell` at `/org/gnome/Shell`** and calls
    `GrabAccelerators(a(suu))→au` / `UngrabAccelerators(au)→b`, listening for
    `AcceleratorActivated(u, a{sv})` / `AcceleratorDeactivated`; that shell interface is documented
    verbatim (with `GrabAccelerator(s,u,u)→u` too) in gnome-settings-daemon's
    `plugins/media-keys/org.gnome.ShellKeyGrabber.xml`
    (https://github.com/GNOME/gnome-settings-daemon/blob/master/plugins/media-keys/org.gnome.ShellKeyGrabber.xml);
  - stores/edits per-app shortcut config via **`org.gnome.Settings.GlobalShortcutsProvider`** at
    `/org/gnome/Settings/GlobalShortcutsProvider`, whose implementation lives in
    gnome-control-center (`global-shortcuts-provider/`, binary
    `gnome-control-center-global-shortcuts-provider` — see
    https://discourse.gnome.org/t/issue-with-global-shortcuts-setup-popup-in-gnome-48/27946 and
    https://github.com/gnome-xorg/gnome-control-center/blob/50.0/global-shortcuts-provider/control-center-global-shortcuts-provider.h);
  - also exports `org.gnome.GlobalShortcutsRebind` at `/org/gnome/globalshortcuts`.
- **Known breakage for non-sandboxed apps (as reported 2026-04 … 2026-09, i.e. GNOME 49–51):**
  - `xdg-desktop-portal-gnome#183`: "Method BindShortcuts is not implemented on interface
    org.gnome.Settings.GlobalShortcutsProvider when using Rust/ASHPD/ZBus" — works for GPU Screen
    Recorder, fails for ashpd/zbus callers; reproduced on GNOME 48 and 49rc1 —
    https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/work_items/183
  - `xdg-desktop-portal-gnome#191`: changed shortcuts are not restored on the next login ("basically
    the global shortcuts work until they are changed… at the next system startups, it won't work
    anymore"), GNOME 49, works on Plasma —
    https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/work_items/191
  - cjpais/Handy#1236 (2026-04-06, GNOME 49 / Fedora 43): shortcuts register successfully but no
    events are delivered; "still has open bugs around non-Flatpak apps with empty `app_id`" —
    https://github.com/cjpais/Handy/issues/1236
  - Chromium's `GlobalShortcutsPortal` feature was reported disabled on GNOME because of popup spam —
    https://issues.chromium.org/issues/404298968 (**[UNVERIFIED]**: the page requires sign-in; only the
    title and search snippet were readable).
- **Answer**: as of GNOME 50/51 the portal *is* implemented and an app *can* register a global
  shortcut, but for a normal (non-Flatpak) binary it is reported to be unreliable (app-id handling
  and persistence bugs) — **[UNVERIFIED]**, my only evidence is the bug reports above, not a live test.

### 2.2 `wlr-layer-shell-unstable-v1` in Mutter — **not implemented**

- Mutter vendors only three protocol XMLs of its own:
  `src/wayland/protocol/{gtk-shell.xml, mutter-x11-interop.xml, xdg-session-management-v1.xml}`
  (https://github.com/GNOME/mutter/tree/main/src/wayland/protocol); everything else comes from
  `wayland-protocols`, which does not contain `zwlr_layer_shell_v1` (the `wlr-*` protocols live in
  wlroots). There is no `meta-wayland-layer-shell*.c` in `src/wayland/`
  (https://github.com/GNOME/mutter/tree/main/src/wayland).
- Third-party statement, same conclusion: gtk4-layer-shell README — "Layer shell **is not
  supported** on: Gnome-on-Wayland" (https://github.com/wmww/gtk4-layer-shell), and the Wayland-2-Gnome
  shim exists precisely because "GNOME Shell (Mutter) does not implement this protocol"
  (https://github.com/leriart/Wayland-2-Gnome).
- **[UNVERIFIED]**: I could not locate a canonical Mutter/GNOME issue stating their position —
  `gitlab.gnome.org` searches for "layer shell"/"wlr-layer" returned no results, and the GitHub
  mutter mirror has issues disabled. Treat the conclusion as well-evidenced by the protocol lists,
  but do not cite a GNOME issue number without checking `gitlab.gnome.org/GNOME/mutter/-/issues`.

### 2.3 Any sanctioned non-extension route?

- **Global hotkey:** only two mechanisms exist without an extension:
  1. the GlobalShortcuts portal (§2.1) — currently buggy for non-Flatpak apps;
  2. a **GNOME custom keybinding**, which *runs a command* (not a D-Bus signal to your app).
     Schema `org.gnome.settings-daemon.plugins.media-keys` key `custom-keybindings` (`as`, list of
     relocatable paths) plus the relocatable schema
     `org.gnome.settings-daemon.plugins.media-keys.custom-keybinding` with keys `name` (`s`),
     `binding` (`s`, GTK accelerator syntax e.g. `<Super>space`), `command` (`s`),
     `enable-in-lockscreen` (`b`) —
     https://github.com/GNOME/gnome-settings-daemon/blob/master/data/org.gnome.settings-daemon.plugins.media-keys.gschema.xml.in
     (this is what Settings → Keyboard → Custom Shortcuts writes; the recommended app-side trick is a
     second invocation of your binary that talks to the running instance over a Unix socket — see the
     Handy issue above, which proposes exactly `handy --toggle`).
  `org.gnome.Shell`'s `GrabAccelerators` is on the session bus but is the interface the shell's own
  clients use; no documented policy for third-party callers was found (**[UNVERIFIED]**).
- **Drawing above windows:** no protocol and no portal. `xdg_toplevel` has no always-on-top request;
  Mutter implements `xdg-activation-v1` (`src/wayland/meta-wayland-activation.c`) only for focus
  tokens, not stacking. Practical consequences: a plain GTK4 window cannot stay above others on
  GNOME Wayland (multiple reports: https://discourse.gnome.org/t/any-way-to-set-window-always-on-top-programmatically/31579),
  and `gtk4-layer-shell`/LayerShellQt-based overlays work on wlroots/KWin/COSMIC but not GNOME.
- **`Meta.WaylandClient`** is a Mutter library API, not an app-facing feature: "A class that allows to
  launch a trusted client and detect if a specific Wayland window belongs to it"
  (`meta_wayland_client_new_subprocess()` sets a private Wayland socket and `WAYLAND_SOCKET`;
  `meta_wayland_client_owns_window()`) — https://mutter.gnome.org/meta/class.WaylandClient.html .
  It gives neither global grabs nor overlay stacking; it is for shell/compositor-side launching.
- **Portals that do exist and are adjacent:** `org.freedesktop.portal.Background` (v2:
  `RequestBackground(parent_window, options)` with options `reason`, `autostart`, `commandline`,
  `dbus-activatable`; `SetStatus`) —
  https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Background.html ;
  `org.freedesktop.portal.GlobalShortcuts`; `org.freedesktop.portal.DynamicLauncher`. There is no
  "overlay"/"always on top" portal in the portal docs index.

### 2.4 Bottom line (§2)

**Confirmed: for a Spotlight-style overlay on GNOME 50/51 Wayland, a GNOME Shell extension is the
only viable route.** A non-extension app can (at best) obtain a global shortcut via the portal or a
custom keybinding command; it cannot draw a persistent overlay above other windows, because Mutter
implements neither `wlr-layer-shell-unstable-v1` nor any always-on-top/overlay capability for
ordinary clients, and no XDG portal provides one. (Corollary, for completeness: GNOME Shell's own JS
is **not** a stable API — extensions declare `shell-version` in `metadata.json` and are checked at
load time (`js/misc/extensionUtils.js`, `loadExtensionMetadata()`, required properties
`uuid`/`name`/`description`/`shell-version`), so the extension half must be maintained per GNOME
release.)

---

## 3. Rust stack for a GNOME-integrated user daemon

### 3.1 zbus vs gio-rs

Current versions (checked 2026-11-01):

- **zbus 6.2.0** (2026-10-29) is latest; 5.19.0 (2026-08-09) was the last 5.x; 4.4.0 last 4.x.
  https://docs.rs/crate/zbus/latest . Since 6.0 the client proxy API and the service/object-server
  API are separate cargo features (`proxy`, `service`, both default; `object-manager` optional);
  `default-features = false` gives wire format + name types only. `zbus_names` was merged into
  `zbus::names` in 6.0 and the standalone crate is unmaintained (RUSTSEC-2026-0371:
  https://rustsec.org/advisories/RUSTSEC-2026-0371.html).
- **gio 0.22.10** (2026-09-19), part of gtk-rs-core; lowest supported GIO is 2.56, MSRV 1.83.
  https://docs.rs/crate/gio/latest . Owns a name via `gio::bus_own_name`
  (= `g_bus_own_name`) and serves objects via `Gio.DBusConnection.register_object`
  (= `g_dbus_connection_register_object`), both documented at
  https://docs.gtk.org/gio/func.bus_own_name.html and
  https://docs.gtk.org/gio/method.DBusConnection.register_object.html .

There is **no official GNOME/gtk-rs recommendation** for zbus-vs-gio. Observable facts:

- The Rust XDG-portal ecosystem (ashpd 0.13.13, 2026-07-17) is built on **zbus**
  (https://docs.rs/crate/ashpd/latest); it has no glib dependency by default (`glib` feature only
  derives `glib::Enum`).
- gio-rs inherits GLib's threading model: callbacks run in the thread-default `GMainContext`
  (GIO overview, "Asynchronous Programming" / "Threads":
  https://docs.gtk.org/gio/overview.html). GDBus also always spawns its own worker thread ("applications
  using GDBus have at least 3 threads").
- zbus is runtime-agnostic: default `zruntime` backend with a helper thread, `tokio` /
  `tokio-multithread` features, or a custom runtime via `connection::Builder::runtime`;
  `zbus::block_on()` for synchronous programs, with the documented caveat that it must not be called
  from the runtime's own loop thread (https://docs.rs/crate/zbus/latest).

Practical split for a daemon that must also read GSettings: you can depend on **both** — zbus for
your own name/service, `glib`+`gio` only for GSettings — but then two event loops exist and must not
block each other (see §3.3).

### 3.2 Reading GSettings/dconf from Rust

- API: `gio::Settings` (gtk-rs) → `Settings::new("org.example.Foo")`, `get_*`/`set_*`,
  `Settings::bind`, signal `changed::<key>`; C reference https://docs.gtk.org/gio/class.Settings.html ,
  Rust walkthrough https://gtk-rs.org/gtk4-rs/stable/latest/book/settings.html .
  Alternative without glib: shell out to `gsettings get|set|monitor` (needs a session bus for writes —
  `gsettings(1)`: "gsettings needs a D-Bus session bus connection to write changes to the dconf
  database", https://man.archlinux.org/man/gsettings.1.en) or `dconf read|write|watch`.
- **Can an external process read a schema installed by a GNOME Shell extension?** Yes.
  - Extension schemas live at
    `~/.local/share/gnome-shell/extensions/<uuid>/schemas/gschemas.compiled`, and the *shell* loads
    them with a private source:
    `Gio.SettingsSchemaSource.new_from_directory(<extdir>/schemas, Gio.SettingsSchemaSource.get_default(), false)`
    then `schemaSource.lookup(id, true)` — exact code in gnome-shell
    `js/extensions/sharedInternals.js` (`ExtensionBase.getSettings()`).
  - `g_settings_schema_source_new_from_directory()` **requires the directory to already contain
    `gschemas.compiled`** (produced by `glib-compile-schemas`) and takes a `trusted` flag (FALSE for
    home-directory schemas) — GLib source `gio/gsettingsschema.c`:
    https://gitlab.gnome.org/GNOME/glib/-/blob/main/gio/gsettingsschema.c .
  - Compilation: since **GNOME 44**, `gnome-extensions install`, extensions.gnome.org and Extension
    Manager compile extension schemas automatically; otherwise run
    `glib-compile-schemas schemas/` (https://gjs.guide/extensions/development/preferences.html).
  - Naming convention: schema id `org.gnome.shell.extensions.<name>`, path
    `/org/gnome/shell/extensions/<name>/` (same page; `settings-schema` key in `metadata.json`).
  - For the daemon: point GSettings at that directory. GLib's `initialise_schema_sources()`
    **prepends** each `GSETTINGS_SCHEMA_DIR` entry (split on `G_SEARCHPATH_SEPARATOR_S`) onto
    `$XDG_DATA_HOME/glib-2.0/schemas` and each `$XDG_DATA_DIRS/glib-2.0/schemas`, each built as
    `trusted=TRUE`; it is ignored when running setuid. GIO documents the variable as: "set to the
    names of directories to consider when looking for compiled schemas… in addition to the
    `glib-2.0/schemas` subdirectories of the XDG system data directories"
    (https://docs.gtk.org/gio/overview.html). In Rust either export
    `GSETTINGS_SCHEMA_DIR=<extdir>/schemas` before first use, or build
    `gio::SettingsSchemaSource::from_directory(...)` yourself and use `gio::Settings::new_full(...)`.
  - Caveat: with the extension schema in a *user-controlled* directory, an external process reading it
    is doing the same "untrusted schema source" dance the shell does — the compiled file must exist
    and must match the installed `.gschema.xml`.

### 3.3 Async runtime

- zbus + **tokio** is clean and well-trodden (zbus `tokio` feature; `tokio-multithread` for a
  multi-threaded runtime; ashpd defaults to tokio).
- GTK/GLib code (including GSettings change notification, and any GTK4 UI) needs a running
  `GMainContext`. Two shapes that work in practice:
  1. tokio drives D-Bus; a dedicated thread owns a `glib::MainContext` (pushed as thread-default) for
     GSettings; cross-thread communication over channels. Requires care: do not call blocking
     `gio` calls (e.g. sync `g_settings_get` is fine; sync D-Bus/GIO I/O is not) from the tokio
     threads, and never call `zbus::block_on` from inside a runtime thread.
  2. Drive everything from GLib's main loop and use an executor that integrates: ashpd's `async-io`
     feature is explicitly documented as "Enable the use of the async-io crates, **compatible with
     runtimes such as smol or glib**" (https://docs.rs/crate/ashpd/latest).
- **[UNVERIFIED]**: no upstream document states a preferred combination; the above is assembled from
  the zbus crate docs, ashpd's feature table, and GIO's threading notes.

### 3.4 LocalSearch from Rust

No crate (§1.5). Either FFI to `libtinysparql-3.0` (pulling in GObject/GLib — cf. §5), shell out to
`tinysparql query` / `localsearch search`, or hand-roll the FD protocol on zbus.

---

## 4. Daemon lifecycle and autostart on GNOME

### 4.1 The pattern GNOME itself uses (LocalSearch 3.12 — best in-tree model)

All three files come from one meson target,
https://github.com/GNOME/localsearch/blob/main/src/indexer/meson.build :

**a) systemd user unit** (`localsearch-3.service`, installed into the user unit dir;
`src/indexer/tracker-miner-fs.service.in`):

```ini
[Unit]
Description=LocalSearch indexer
ConditionUser=!@system
ConditionEnvironment=XDG_SESSION_CLASS=user
After=gnome-session.target

[Service]
Type=notify
BusName=org.freedesktop.LocalSearch3
ExecStart=@libexecdir@/localsearch-3
Restart=on-failure
Slice=background.slice
```

(Note: `Type=notify` *and* `BusName=`; `Slice=background.slice`; `After=gnome-session.target`.)

**b) D-Bus session activation file** (`/usr/share/dbus-1/services/org.freedesktop.LocalSearch3.service`,
from `src/indexer/org.freedesktop.Tracker3.Miner.Files.service.in`):

```ini
[D-BUS Service]
Name=org.freedesktop.LocalSearch3
Exec=@libexecdir@/localsearch-3
SystemdService=localsearch-3.service
```
(a second identical file is generated for the legacy name `org.freedesktop.Tracker3.Miner.Files`,
and there are `.Control`/`.Writeback` variants.)

**c) XDG-autostart fallback** (`/etc/xdg/autostart/localsearch-3.desktop`,
from `src/indexer/tracker-miner-fs.desktop.in`): `Type=Application`,
`X-GNOME-Autostart-enabled=true`, **`X-GNOME-HiddenUnderSystemd=true`**, **`X-systemd-skip=true`**,
`OnlyShowIn=GNOME;KDE;XFCE;X-IVI;Unity;`, `NoDisplay=true`. The meson comment says it plainly:
*"This file allows starting the service automatically using XDG autostart. systemd user service
activation is preferred if available."*

### 4.2 Mechanics and primary docs

- D-Bus activation: dbus-daemon(1) §"INTEGRATING SESSION SERVICES" — service file with `Name=`,
  `Exec=`, optional `SystemdService=`; activation happens "either by running the session service
  itself or by communicating with **systemd --user**"; the standard session service directories are
  `$XDG_RUNTIME_DIR/dbus-1/services` (transient, strict naming), `$XDG_DATA_HOME/dbus-1/services`
  (defaults to `~/.local/share/dbus-1/services`), each `<dir>/dbus-1/services` for `XDG_DATA_DIRS`
  (defaults `/usr/local/share:/usr/share`), and `${datadir}/dbus-1/services`; `--systemd-activation`
  enables systemd-style activation —
  https://dbus.freedesktop.org/doc/dbus-daemon.1.html
  So a user-level daemon can ship either `~/.local/share/dbus-1/services/<Name>.service` or a
  distro-level `/usr/share/dbus-1/services/<Name>.service`.
- systemd side: `Type=dbus` requires `BusName=` and is the default when `BusName=` is set; the unit
  counts as started when the name is acquired and is **terminated when the name is released**;
  `Type=dbus` units implicitly gain `Requires=`/`After=dbus.socket`; for bus-activatable services the
  guidance is to not add an `[Install]` section and to use `SystemdService=` in the D-Bus service
  file — https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html (§Type=,
  §BusName=, Example 7). `Type=notify` (+ `sd_notify`) is the alternative readiness protocol;
  `notify-reload` if you want SIGHUP reloads with completion notification.
- Session targets: `graphical-session.target` is a *passive* user target "active whenever any
  graphical session is running"; services that belong to a session should set
  `PartOf=graphical-session.target`; a session target such as `gnome-session.target` starts/stops it
  with `BindsTo=graphical-session.target` — the man page's own example is
  `gnome-session.target` + `nautilus.service`. Slices: `session.slice` (essential session services,
  "includes the display server, screen readers and other services such as DBus or XDG portals"),
  `app.slice`, `background.slice` ("non-interactive tasks like file indexing or backup operations
  where latency is not important") — https://www.freedesktop.org/software/systemd/man/latest/systemd.special.html
- XDG autostart as the systemd-managed path: `systemd-xdg-autostart-generator` generates user units
  from `~/.local/config/autostart`/`/etc/xdg/autostart` desktop files; DEs opt in via
  `Wants=xdg-desktop-autostart.target`. Keys that suppress generation: `Hidden=` and
  **`X-systemd-skip=`** ("No service will be generated if set to true"),
  `X-GNOME-Autostart-Phase=` ("No service will be generated if set to any value"), `TryExec=`;
  `OnlyShowIn=`/`NotShowIn=`/`AutostartCondition=`/`X-KDE-autostart-condition=` become
  `ExecCondition=` — https://www.freedesktop.org/software/systemd/man/latest/systemd-xdg-autostart-generator.html
  Autostart spec (directories, `Hidden`, `OnlyShowIn`/`NotShowIn`, `TryExec`):
  https://specifications.freedesktop.org/autostart-spec/latest/
- From the extension side, "activation on demand" is just a method call to the well-known name:
  `Gio.DBusProxy.new_for_bus(...)` starts the service (unless
  `Gio.DBusProxyFlags.DO_NOT_AUTO_START` is passed — xdg-desktop-portal-gnome deliberately uses that
  flag for `org.gnome.Shell`); name-ownership changes are watched with
  `Gio.bus_watch_name()` / `notify::g-name-owner` —
  https://gjs.guide/guides/gio/dbus.html .
  GNOME's own D-Bus-activation guidance: send a message to the well-known name;
  `DBusActivatable=true` in the `.desktop` file makes GIO ignore `Exec=` and activate over D-Bus;
  "Being D-Bus activatable is a prerequisite for using persistent notifications" —
  https://developer.gnome.org/documentation/guidelines/maintainer/integrating.html#d-bus-activation

### 4.3 Non-systemd GNOME distros (Alpine, Void, …)

- **D-Bus activation still works without systemd.** dbus-daemon can activate by running `Exec=`
  itself; `SystemdService=` is an optional extension that is simply irrelevant when there is no
  systemd user manager (dbus-daemon(1), §4.2). So `~/.local/share/dbus-1/services/<Name>.service`
  (with `Exec=`) is the portable activation mechanism.
- What breaks without systemd: `Type=dbus`/`BusName=` units don't exist, `graphical-session.target` /
  `PartOf=` semantics don't apply, and `systemd-xdg-autostart-generator` is absent.
- Fallback: **XDG autostart** — ship the same `.desktop` file without `X-systemd-skip=true` /
  `X-GNOME-HiddenUnderSystemd=true`, installed to `/etc/xdg/autostart/` or
  `~/.config/autostart/`; gnome-session and other DEs read those directories per the autostart spec
  (this is exactly why localsearch ships `localsearch-3.desktop`). Second fallback: the shell
  extension spawns the daemon itself (`Gio.Subprocess`) and relies on the D-Bus name as the
  single-instance lock.
- **[UNVERIFIED]**: no GNOME doc formally specifies a non-systemd session-management path; the
  evidence is the freedesktop autostart spec plus the LocalSearch packaging above.

### 4.4 Preventing two copies

- Own the well-known name **without** queueing or replacing: `Gio.bus_own_name(..., BusNameOwnerFlags.NONE, ...)`
  (GJS: https://gjs.guide/guides/gio/dbus.html) / `g_bus_own_name`; in zbus use
  `connection::Builder::name(...)` or call `org.freedesktop.DBus.RequestName` yourself. The D-Bus
  spec's `RequestName` semantics (flags `DO_NOT_QUEUE`, `REPLACE_EXISTING`; replies
  `PRIMARY_OWNER` / `IN_QUEUE` / `EXISTS` / `ALREADY_OWNER`) are the normative single-instance
  mechanism — https://dbus.freedesktop.org/doc/dbus-specification.html#message-bus-methods
- With a systemd user unit, `Type=dbus` + `BusName=` also guarantees single-instance at the systemd
  level; a copy started outside systemd loses the name race and should exit non-zero
  (systemd.service(5), §BusName=).
- Belt-and-braces: an `flock()` on a file under `$XDG_RUNTIME_DIR` distinguishes "another copy still
  starting up" from "another copy already serving".

### 4.5 Shell restart vs logout

- **Shell restart / extension reload**: the extension runs inside `gnome-shell`; `gnome-shell --replace`
  or a disable→enable cycle tears down the extension's objects but **not** your separate daemon, which
  keeps its session-bus name and its state. The extension must re-create proxies in `enable()` and
  should tolerate the daemon's absence (watch `notify::g-name-owner`, `Gio.bus_watch_name`). The daemon
  must not treat "a client disconnected" as "exit" — do not tie its lifetime to a client's unique
  name.
- **Logout**: with systemd, session-scoped user units stop when `graphical-session.target` stops
  (`PartOf=`), and the user manager (`systemd --user`) itself exits when the last session ends unless
  lingering is enabled (`loginctl enable-linger`). Whether processes in the *session cgroup* are killed
  is governed by logind's `KillUserProcesses=` — the upstream default is `yes`
  (https://www.freedesktop.org/software/systemd/man/latest/logind.conf.html;
  Fedora deliberately overrides it to `no`:
  https://fedoraproject.org/wiki/Changes/KillUserProcesses_by_default). Handle `SIGTERM` and flush
  state (LocalSearch pairs this with `Restart=on-failure` and `Type=notify`).
- Also: a `Type=dbus` service receives `SIGTERM` (or `KillSignal=`) if it releases its bus name
  (systemd.service(5)) — i.e. "drop the name" is effectively "shut down".

---

## 5. Rust static (musl) distribution

### 5.1 Targets and toolchain

- `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` are **Tier 2 with host tools**; since
  Rust **1.93** (2026-01-22) all `*-linux-musl` targets ship **musl 1.2.5** and require musl ≥ 1.2.5
  (the other three Tier-2-with-host-tools musl targets, incl. `powerpc64le-…-musl`, were previously
  pinned to 1.2.3). Breaking change in that update: removal of legacy symbols (`open64` etc.) that
  very old `libc`/`getrandom` versions referenced — fix by `cargo update`
  (https://blog.rust-lang.org/2025/12/05/Updating-musl-1.2.5/ ;
  https://doc.rust-lang.org/rustc/platform-support/aarch64-unknown-linux-musl.html ;
  https://doc.rust-lang.org/rustc/platform-support.html).
- Build:
  ```
  rustup target add x86_64-unknown-linux-musl
  cargo build --release --target x86_64-unknown-linux-musl
  ```
  The musl targets are statically linked by default today; `-C target-feature=+crt-static` is the
  explicit switch. **[UNVERIFIED / watch item]**: there is a long-standing proposal to make the musl
  targets dynamically linked by default (rust-lang/compiler-team#422) — it had not shipped as of the
  2025-12 blog post, but if it lands, `+crt-static` becomes mandatory for a static binary.
- Cross-building aarch64 from x86_64 needs a musl cross-linker
  (`aarch64-linux-musl-gcc`, e.g. musl.cc or `messense/musl-cross` images) wired up via
  `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER`; the `cross` tool (cross-rs) automates the
  container plumbing. **[UNVERIFIED]**: standard practice, not stated in the Rust book pages above.

### 5.2 Can a static binary still use GSettings/dconf and D-Bus?

Runtime-wise **yes** — dconf and D-Bus are services/protocols, not link-time dependencies. But:

- **D-Bus via zbus**: pure Rust socket/authentication code, no C library; works in a static binary
  (zbus requires `DBUS_SESSION_BUS_ADDRESS` or defaults — see the zbus crate docs).
- **GSettings' dconf backend is a loadable GIO module.** GIO's extension points are "implementations
  [that] get loaded from modules (executable code in shared objects)"; `GSETTINGS_BACKEND=dconf`
  selects "the one in dconf", and module discovery uses the compiled-in module dir, overridable with
  `GIO_MODULE_DIR` / `GIO_EXTRA_MODULES` — https://docs.gtk.org/gio/overview.html and
  https://docs.gtk.org/gio/class.SettingsBackend.html . **So a statically linked GIO still needs the
  host's `libdconfsettings.so` (package `dconf`) at runtime** to share settings with the shell/extension.
  Escape hatches: `GSETTINGS_BACKEND=keyfile` or `=memory` (but then you are not sharing dconf state).

### 5.3 Is static glib painful? Yes — and here is why

- The gtk-rs `-sys` crates resolve GLib/GIO at **build time via pkg-config**: `glib-sys`/`gio-sys`
  depend on `system-deps`, which documents that it "lets you write system dependencies in Cargo.toml
  metadata", supports "only pkg-config dependencies", and that "-sys crates [can] optionally
  internally build and static link the required system library"
  (https://docs.rs/crate/system-deps/1.2.0 , https://github.com/gdesmott/system-deps).
  That means: for a musl/musl cross target you must supply a **musl-built glib + GIO** for pkg-config
  to find, i.e. cross-build glib (`meson -Ddefault_library=static`) and all of its dependencies
  (pcre2, libffi, libmount, libselinux/zlib where used, …) statically for that target. GLib's own
  build docs require meson + pkg-config + a compliant C toolchain
  (https://docs.gtk.org/glib/building.html) and say nothing about supporting a fully static deployment.
- GLib's own design assumes dynamic modules (GIO extension points, `GIO_MODULE_DIR`), so "static glib"
  is a partially-supported configuration even when it links: the modules still come from the host.
- Additional implicit cost: linking libtinysparql/libtracker-sparql for LocalSearch queries brings its
  own dynamic modules (`/usr/lib/localsearch-3.0/extract-modules/*.so`,
  `writeback-modules/*.so`, `/usr/lib/tinysparql-3.0/libtracker-*.so` — see the Arch file list), so a
  static daemon that *does* LocalSearch queries is not self-contained anyway.
- **[UNVERIFIED]**: no upstream GLib/gtk-rs statement endorsing or forbidding static builds was found;
  the conclusion above is derived from the pkg-config-based build, the module/extension-point docs and
  the packaging file lists. No static build was actually performed for this note.

### 5.4 Alternatives if static glib is unacceptable

1. **Static core, no glib at all**: pure-Rust daemon (zbus for its name/service; dconf/keyfile via a
   small helper, `dconf read|write`, or a `GSETTINGS_BACKEND=keyfile` file) → a genuinely
   dependency-free static binary, at the cost of not using GSettings/dconf natively.
2. **Dynamic against the host GLib/GIO** (the normal GNOME expectation): distro package, or Flatpak
   runtime. Flatpak/`org.gnome.Platform` ships TinySPARQL (`sandboxing.html`).
3. Bundle the GNOME runtime (Flatpak) instead of static linking.

---

## Flagged / uncertain items (consolidated)

1. `TrackerSparqlValueType` enum numeric values — **not read** (`tracker-enums.h`).
2. Whether `gdbus introspect` on a live GNOME 50/51 shows the endpoint interface — inferred from the
   source (`g_dbus_connection_register_object` with parsed node info); not executed.
3. CLI stability: no explicit upstream stability statement found either way; evidence of churn is real
   (`tracker3` → `tinysparql`; docs still say `tinysparql3`/`localsearch3`).
4. GlobalShortcuts behaviour for non-Flatpak apps on GNOME 50/51 — only bug reports (#183, #191,
   cjpais/Handy#1236) and a Chromium issue whose page requires sign-in; no live test.
5. No canonical GNOME/Mutter issue number for "we do not implement wlr-layer-shell" was found
   (gitlab.gnome.org searches returned nothing; the GitHub mutter mirror has issues disabled).
6. `org.gnome.Shell`'s `GrabAccelerator(s)` availability policy for third-party callers — undocumented
   in anything I could read.
7. Async-runtime preference (tokio vs GLib loop) — no upstream guidance; assembled from crate docs.
8. `-C target-feature=+crt-static` default for musl targets "may change" per rust-lang/compiler-team#422.
9. Static-glib viability — reasoned, not tested.

*Assumption:* the `spotlight` repo had no existing research-notes convention (`docs/` contained only
`docs/agents/`), so this note was written to `docs/research/`.
