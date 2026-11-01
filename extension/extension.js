import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {LumenOverlay} from './overlay.js';
import {LumenSystem} from './system.js';
import {EngineStatus, engineState, ENGINE_NOT_RUNNING} from './lib/engine.js';
import {
    A11Y_SCHEMA,
    ANIMATIONS_KEY,
    HIGH_CONTRAST_KEY,
    INTERFACE_SCHEMA,
    REDUCED_MOTION_KEY,
    accessibilityState,
} from './lib/a11y.js';
import {BUS_NAME, OBJECT_PATH, INTERFACE, SUPPORTED_MAJOR} from './lib/protocol.js';
import {
    DEFAULT_RESULT_LIMIT,
    buildRequest,
    clampResultLimit,
    hasControlModifier,
    hasSecondWindowModifier,
} from './lib/query.js';
import {activationAction} from './lib/activation.js';
import {buildViewModel} from './lib/viewmodel.js';
import {createNavigator, sectionStarts} from './lib/navigation.js';
import {DEFAULT_PAGE_SIZE, hasShift, intentFor} from './lib/keys.js';
import {appCandidates, appIdFromResultId} from './lib/apps.js';
import {
    ACTION_PROVIDER,
    SETTINGS_PROVIDER,
    actionCandidates,
    actionVerbFromResultId,
    settingKeyFromResultId,
    toggleCandidates,
} from './lib/actions.js';
import {resultsFromRows, unpack} from './lib/wire.js';
import {createClipboardSampler, shouldCapture, SAMPLE_INTERVAL_SECONDS} from './lib/clipboard.js';
import {updateNotification, updateStatus, versionFilePaths} from './lib/update.js';

const SUMMON_KEYBINDING = 'summon-shortcut';
const PROVIDERS_KEY = 'providers-enabled';
const APPEARANCE_KEY = 'appearance';
const CLIPBOARD_KEY = 'clipboard-enabled';
const RESULT_LIMIT_KEY = 'result-limit';
const APPLICATION_PROVIDER = 'applications';
// The one provider whose rows are paged by count, so the offset of the next page is how many of
// them are already loaded (contract §11).
const FILE_PROVIDER = 'files';
// Providers the shell resolves itself: the daemon ranks them, but only the shell can run them.
const SHELL_PROVIDERS = new Set([ACTION_PROVIDER, SETTINGS_PROVIDER]);
// The `delete` action a clipboard entry carries (§11): the Delete key is what runs it, and
// Control+Delete clears the whole history. Distinct from a row's id so the arm cannot be confused
// with the destructive rows `_activateSystem` confirms.
const DELETE_ACTION = 'delete';
const CLEAR_CLIPBOARD = 'clear-clipboard';

export default class LumenExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._shutdown = false;
        this._proxy = null;
        this._proxySignals = [];
        this._owner = null;
        this._keybindingBound = false;
        this._engine = {status: null, message: ENGINE_NOT_RUNNING};
        this._activeSearch = null;
        this._awaitingResults = false;
        this._results = [];
        // The daemon's own results for the query on screen, and how many of them are file rows —
        // the offset of the next page (contract §11).
        this._loaded = [];
        this._loadedFiles = 0;
        this._navigator = null;
        this._apps = null;
        this._clipboard = St.Clipboard.get_default();
        this._sampler = createClipboardSampler();
        this._captureTimerId = 0;
        this._confirmation = null;
        this._interfaceSettings = null;
        this._a11ySettings = null;
        this._animationsChangedId = 0;
        this._reducedMotionChangedId = 0;
        this._highContrastChangedId = 0;

        this._overlay = new LumenOverlay({
            onClose: () => this._onOverlayClosed(),
            onActivate: index => this._onRowActivated(index),
            onKey: (keyval, modifiers) => this._onKey(keyval, modifiers),
        });
        this._setEngineState(null, null);

        // Is there a newer release than the one installed? (ticket 16)
        this._checkForUpdate();

        // The system actions and setting toggles come from the shell itself (ticket 09): their live
        // state is gathered here, and `onChanged` re-queries when something changes the state
        // elsewhere — the quick settings menu, `gsettings`, a hardware switch.
        this._system = new LumenSystem({
            onChanged: () => this._onSystemStateChanged(),
            onError: message => this._reportFailure(message),
        });

        this._textChangedId = this._overlay.entry.clutter_text.connect(
            'text-changed', () => this._onQueryChanged());

        this._shortcutChangedId = this._settings.connect(
            `changed::${SUMMON_KEYBINDING}`, () => this._bindShortcut());
        this._bindShortcut();

        // The preferences window (ticket 12) writes to this schema from another process, so the
        // three preferences that change what the running shell does are observed here: the
        // appearance is applied to the overlay, the provider list re-runs the query on screen, and
        // the clipboard switch is pushed to the engine and starts or stops sampling.
        this._appearanceChangedId = this._settings.connect(
            `changed::${APPEARANCE_KEY}`, () => this._applyAppearance());
        this._providersChangedId = this._settings.connect(
            `changed::${PROVIDERS_KEY}`, () => this._refreshQuery());
        this._clipboardChangedId = this._settings.connect(
            `changed::${CLIPBOARD_KEY}`, () => this._onClipboardSettingChanged());
        this._applyAppearance();

        // Accessibility (ticket 14). Reduced motion and high contrast are the desktop's settings,
        // not Lumen's: they are read here, observed, and handed to the overlay, which applies them
        // to whatever it has built. Two schemas — `org.gnome.desktop.a11y.interface` owns
        // `high-contrast` and the GNOME 50 `reduced-motion` enum, while the global animation switch
        // `enable-animations` lives in `org.gnome.desktop.interface` (both are honoured; see
        // lib/a11y.js).
        this._interfaceSettings = this._openDesktopSettings(INTERFACE_SCHEMA);
        this._a11ySettings = this._openDesktopSettings(A11Y_SCHEMA);
        this._animationsChangedId = this._interfaceSettings?.connect(
            `changed::${ANIMATIONS_KEY}`, () => this._applyAccessibility()) ?? 0;
        this._reducedMotionChangedId = this._a11ySettings?.connect(
            `changed::${REDUCED_MOTION_KEY}`, () => this._applyAccessibility()) ?? 0;
        this._highContrastChangedId = this._a11ySettings?.connect(
            `changed::${HIGH_CONTRAST_KEY}`, () => this._applyAccessibility()) ?? 0;
        this._applyAccessibility();

        this._watcherId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            (_connection, _name, owner) => this._onEngineAppeared(owner),
            () => this._onEngineVanished());
    }

    disable() {
        this._shutdown = true;

        this._stopClipboardCapture();

        if (this._overlay && this._textChangedId) {
            this._overlay.entry.clutter_text.disconnect(this._textChangedId);
            this._textChangedId = 0;
        }

        if (this._shortcutChangedId) {
            this._settings?.disconnect(this._shortcutChangedId);
            this._shortcutChangedId = 0;
        }

        for (const id of [
            this._appearanceChangedId, this._providersChangedId, this._clipboardChangedId,
        ]) {
            if (id)
                this._settings?.disconnect(id);
        }
        this._appearanceChangedId = 0;
        this._providersChangedId = 0;
        this._clipboardChangedId = 0;
        this._settings = null;

        for (const [settings, id] of [
            [this._interfaceSettings, this._animationsChangedId],
            [this._a11ySettings, this._reducedMotionChangedId],
            [this._a11ySettings, this._highContrastChangedId],
        ]) {
            if (id)
                settings.disconnect(id);
        }
        this._animationsChangedId = 0;
        this._reducedMotionChangedId = 0;
        this._highContrastChangedId = 0;
        this._interfaceSettings = null;
        this._a11ySettings = null;

        if (this._keybindingBound) {
            Main.wm.removeKeybinding(SUMMON_KEYBINDING);
            this._keybindingBound = false;
        }

        if (this._watcherId) {
            Gio.bus_unwatch_name(this._watcherId);
            this._watcherId = 0;
        }

        this._dropProxy();
        this._system?.destroy();
        this._system = null;
        this._overlay?.destroy();
        this._overlay = null;
        this._navigator = null;
        this._results = [];
        this._loaded = [];
        this._loadedFiles = 0;
        this._apps = null;
        this._clipboard = null;
        this._sampler = null;
        this._confirmation = null;
    }

    // The summon keybinding. mutter only reads a keybinding from a string-array key, so
    // `summon-shortcut` is `as` and holds exactly one accelerator here.
    _bindShortcut() {
        if (this._keybindingBound) {
            Main.wm.removeKeybinding(SUMMON_KEYBINDING);
            this._keybindingBound = false;
        }

        const [accelerator = ''] = this._settings?.get_strv(SUMMON_KEYBINDING) ?? [];
        if (!accelerator)
            return;

        const action = Main.wm.addKeybinding(
            SUMMON_KEYBINDING,
            this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.ALL,
            () => this._toggleOverlay());
        this._keybindingBound = action !== Meta.KeyBindingAction.NONE;
        log(`Lumen: summon shortcut ${this._keybindingBound ? 'bound to' : 'NOT bound:'} ${accelerator}`);
    }

    // The `appearance` preference themes the overlay itself (system / light / dark). The launcher's
    // dark-mode toggle drives the desktop's colour scheme instead (§7d), which `system` follows.
    _applyAppearance() {
        this._overlay?.setAppearance(this._settings?.get_string(APPEARANCE_KEY) ?? 'system');
    }

    // A desktop schema that is not installed is not a failure: the overlay is perfectly usable
    // without it, and `accessibilityState` then keeps GNOME's own defaults.
    _openDesktopSettings(schemaId) {
        try {
            return new Gio.Settings({schema_id: schemaId});
        } catch (error) {
            logError(error, `Lumen: no ${schemaId}, keeping the desktop's defaults`);
            return null;
        }
    }

    // The accessibility preferences, folded by lib/a11y.js into the two booleans the overlay
    // applies: motion, and high contrast. Re-run whenever one of them changes, so a running
    // overlay stops animating the moment the preference lands.
    _applyAccessibility() {
        const state = accessibilityState({
            enableAnimations: this._readBoolean(this._interfaceSettings, ANIMATIONS_KEY),
            reducedMotion: this._readString(this._a11ySettings, REDUCED_MOTION_KEY),
            highContrast: this._readBoolean(this._a11ySettings, HIGH_CONTRAST_KEY),
        });

        this._overlay?.setAccessibility(state);
    }

    // A key that is missing, or of a type this build does not expect, is left unanswered rather
    // than guessed at.
    _readBoolean(settings, key) {
        try {
            return settings?.get_boolean(key);
        } catch {
            return undefined;
        }
    }

    _readString(settings, key) {
        try {
            return settings?.get_string(key);
        } catch {
            return undefined;
        }
    }

    _toggleOverlay() {
        if (this._overlay.isOpen) {
            this._overlay.close();
            return;
        }

        if (Main.modalCount > 0) {
            log('Lumen: a shell dialog is open, not summoning the overlay');
            return;
        }

        log('Lumen: summoning the overlay');
        this._apps = null;
        this._overlay.open();
    }

    _onEngineAppeared(owner) {
        this._owner = owner;

        if (this._proxy)
            return;

        Gio.DBusProxy.new_for_bus(
            Gio.BusType.SESSION,
            Gio.DBusProxyFlags.NONE,
            null,
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            null,
            (_source, result) => this._onProxyReady(result, owner));
    }

    _onProxyReady(result, owner) {
        if (this._shutdown || this._owner !== owner)
            return;

        let proxy;
        try {
            proxy = Gio.DBusProxy.new_for_bus_finish(result);
        } catch (error) {
            logError(error, 'Lumen: could not connect to the engine');
            this._setEngineState(null, null);
            return;
        }

        this._proxy = proxy;
        // Gio.DBusProxy.connectSignal() returns an id but never fires on GJS 1.88.1, so the
        // engine's signals come through 'g-signal' and are filtered by name here.
        this._proxySignals = [
            proxy.connect('g-properties-changed', () => this._readVersion()),
            proxy.connect('notify::g-name-owner', () => this._readVersion()),
            proxy.connect('g-signal', (_proxy, _sender, signalName, parameters) => {
                if (signalName === 'ResultsChanged')
                    this._onResultsChanged(parameters);
                else if (signalName === 'ClipboardChanged')
                    this._onClipboardChanged();
                else if (signalName === 'ToggleRequested')
                    this._toggleOverlay();
            }),
        ];
        this._readVersion();

        // The engine's own history flag (§2) is what it restored from its last run, so the
        // preference the user sees is pushed over it as soon as the engine is reachable.
        this._pushClipboardEnabled();
    }

    _onEngineVanished() {
        this._owner = null;
        this._dropProxy();
        this._setEngineState(null, null);
    }

    _readVersion() {
        const owner = this._proxy?.g_name_owner ?? null;
        if (!owner) {
            this._setEngineState(null, null);
            return;
        }

        const version = this._proxy.get_cached_property('Version')?.deep_unpack() ?? null;
        this._setEngineState(owner, version);
    }

    _setEngineState(nameOwner, version) {
        const state = engineState({nameOwner, version, supported: SUPPORTED_MAJOR});
        const changed = state.status !== this._engine.status;
        this._engine = state;

        if (changed)
            log(`Lumen: engine ${state.status} (${state.message})`);

        this._overlay?.setEngineState(state);
        this._syncClipboardCapture();

        if (state.status !== EngineStatus.OK) {
            this._loaded = [];
            this._loadedFiles = 0;
            this._render([]);
        } else if (changed && this._overlay?.entry.get_text()) {
            this._onQueryChanged();
        }
    }

    _dropProxy() {
        const proxy = this._proxy;
        this._proxy = null;

        for (const id of this._proxySignals)
            proxy?.disconnect(id);
        this._proxySignals = [];
        this._activeSearch = null;
        this._awaitingResults = false;
        this._syncClipboardCapture();
    }

    // The update path (ticket 16). Lumen is updated by re-running the installer, so there is
    // nothing to fetch here: the installer writes the version it installed and the newest release
    // it knows about into the Lumen data directory, and this compares the two. A git checkout (no
    // data directory), a missing file and an unreadable one are all "no news", never an error —
    // and nothing here touches the network (contract §9).
    _checkForUpdate() {
        const paths = versionFilePaths({
            xdgDataHome: GLib.get_user_data_dir(),
            home: GLib.get_home_dir(),
        });
        const status = updateStatus(
            this._readTextFile(paths.installed),
            this._readTextFile(paths.latest));

        const notification = updateNotification(status);
        if (notification)
            Main.notify(notification.title, notification.body);
        else
            log(`Lumen ${status.current ?? 'version unknown'} (${status.state})`);
    }

    _readTextFile(path) {
        if (!path)
            return null;

        try {
            const [ok, bytes] = GLib.file_get_contents(path);
            return ok ? new TextDecoder().decode(bytes) : null;
        } catch {
            return null;
        }
    }

    // Clipboard capture. The spec calls for St.Clipboard's `owner-changed` signal, which GNOME 50's
    // St does not have (no StClipboard signals are registered at all), so the clipboard is sampled
    // on a timer while — and only while — history is on.
    //
    // Two switches have to agree before anything is read (user story 43): the engine's own
    // `ClipboardEnabled` (§11), which is what makes a push worth sending, and the user's
    // `clipboard-enabled` preference, which is what they see. The preference is pushed to the
    // engine whenever it changes and whenever the engine appears, so an engine that came up with
    // history enabled from a previous session cannot record a copy the preference no longer wants.
    _clipboardEnabled() {
        if (this._settings?.get_boolean(CLIPBOARD_KEY) !== true)
            return false;

        return this._proxy?.get_cached_property('ClipboardEnabled')?.deep_unpack() === true;
    }

    _onClipboardSettingChanged() {
        this._pushClipboardEnabled();
        this._syncClipboardCapture();
    }

    // `ClipboardSetEnabled(enabled)` (§2). The engine answers by changing its `ClipboardEnabled`
    // property, which comes back over `g-properties-changed` and starts or stops the sampling.
    _pushClipboardEnabled() {
        if (this._shutdown || !this._proxy || this._engine.status !== EngineStatus.OK)
            return;

        const enabled = this._settings?.get_boolean(CLIPBOARD_KEY) === true;
        const call = new GLib.Variant('(b)', [enabled]);
        this._proxy.call('ClipboardSetEnabled', call, Gio.DBusCallFlags.NONE, -1, null,
            (proxy, result) => {
                try {
                    proxy.call_finish(result);
                } catch (error) {
                    logError(error, 'Lumen: the engine refused the clipboard setting');
                }
            });
    }

    _syncClipboardCapture() {
        const wanted = shouldCapture({
            enabled: this._clipboardEnabled(),
            engineReady: this._engine.status === EngineStatus.OK && this._proxy !== null,
        });

        if (wanted && !this._captureTimerId) {
            this._captureTimerId = GLib.timeout_add_seconds(
                GLib.PRIORITY_DEFAULT,
                SAMPLE_INTERVAL_SECONDS,
                () => {
                    this._sampleClipboard();
                    return GLib.SOURCE_CONTINUE;
                });
        } else if (!wanted && this._captureTimerId) {
            this._stopClipboardCapture();
        }
    }

    _stopClipboardCapture() {
        if (!this._captureTimerId)
            return;

        GLib.source_remove(this._captureTimerId);
        this._captureTimerId = 0;
        this._sampler?.reset();
    }

    _sampleClipboard() {
        if (this._shutdown || !this._clipboard)
            return;

        this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clipboard, text) => {
            if (this._shutdown)
                return;

            const payload = this._sampler?.sample(text ?? '', Date.now());
            if (payload)
                this._pushClipboard(payload);
        });
    }

    _pushClipboard(payload) {
        if (!this._proxy)
            return;

        const call = new GLib.Variant('(st)', [payload.text, payload.timestamp]);
        this._proxy.call('ClipboardPush', call, Gio.DBusCallFlags.NONE, -1, null, (_proxy, result) => {
            try {
                _proxy.call_finish(result);
            } catch (error) {
                logError(error, 'Lumen: the engine refused the clipboard entry');
            }
        });
    }

    // A new copy is a new result: an open list re-runs the query so it is on screen.
    _onClipboardChanged() {
        if (!this._overlay?.isOpen || this._overlay.entry.get_text() === '')
            return;

        this._onQueryChanged();
    }

    // The candidates the shell computes itself and hands to the daemon to rank: applications, the
    // system actions and the setting toggles. A provider the user switched off is not injected.
    _injectedCandidates(request) {
        const enabled = new Set(request.enabled);
        const injected = [];

        if (enabled.has(APPLICATION_PROVIDER))
            injected.push(...appCandidates(this._applicationCandidates(), request.text, {now: Date.now()}));

        if (enabled.has(ACTION_PROVIDER))
            injected.push(...actionCandidates(request.text, {actions: this._system?.actions}));

        if (enabled.has(SETTINGS_PROVIDER))
            injected.push(...toggleCandidates(request.text, {toggles: this._system?.toggles}));

        return injected;
    }

    // Wi-Fi, Bluetooth, Do Not Disturb, the theme, a lock/suspend availability policy, or the list
    // of enabled providers changed outside the overlay: search again so the rows on screen are the
    // ones the shell would show now.
    _onSystemStateChanged() {
        this._refreshQuery();
    }

    // Re-run the query that is on screen, if there is one. Used by everything a result was built
    // from and that can change underneath it — the system state and the provider preference.
    _refreshQuery() {
        if (this._shutdown || !this._overlay?.isOpen)
            return;

        if (this._overlay.entry.get_text().trim() === '')
            return;

        this._onQueryChanged();
    }

    _onOverlayClosed() {
        this._confirmation = null;
    }

    // The query pipeline: shell-owned candidates go to the daemon, which ranks them with its own.
    // `offset` asks for a later page of the daemon's own paging providers (contract §11); the first
    // page of a new query starts from scratch, and a page is merged into what is already loaded.
    _onQueryChanged({offset = 0} = {}) {
        this._activeSearch = null;
        this._awaitingResults = false;
        this._confirmation = null;

        const request = buildRequest(
            this._overlay?.entry.get_text() ?? '',
            this._settings?.get_strv(PROVIDERS_KEY) ?? [],
        );

        if (offset === 0) {
            this._loaded = [];
            this._loadedFiles = 0;
        }

        if (request.text === '') {
            this._overlay?.setStatus(this._engine.message);
            this._render([]);
            return;
        }

        const injected = this._injectedCandidates(request);

        if (this._engine.status !== EngineStatus.OK || !this._proxy) {
            this._render(injected);
            return;
        }

        const options = {
            // `injected` is an `aa{sv}`, so it must be a variant: a bare JS array makes the packer
            // throw ("not a subclass of GObject_Struct, it's a Array") before Query is even sent,
            // which leaves the overlay with no results at all.
            injected: new GLib.Variant('aa{sv}', variantsOf(injected)),
            limit: new GLib.Variant('u', clampResultLimit(this._settings?.get_int(RESULT_LIMIT_KEY))),
        };
        if (offset > 0)
            options.offset = new GLib.Variant('u', offset);

        const call = new GLib.Variant('(sasa{sv})', [request.text, request.enabled, options]);

        this._awaitingResults = true;
        this._proxy.call('Query', call, Gio.DBusCallFlags.NONE, -1, null, (proxy, result) => {
            try {
                const [searchId] = proxy.call_finish(result).deep_unpack();
                this._activeSearch = searchId;
            } catch (error) {
                logError(error, 'Lumen: the engine refused the query');
                this._overlay?.setStatus('the engine refused the query');
            }
        });
    }

    // PageDown on a list that is already showing results asks for the next page: the offset is how
    // many file rows are on screen, which is where the daemon's file provider left off, and the
    // page is merged into them. Without an engine there is nothing to page from.
    _requestNextPage() {
        if (this._shutdown || !this._overlay?.isOpen)
            return;

        if (this._engine.status !== EngineStatus.OK || !this._proxy)
            return;

        if (this._loadedFiles === 0)
            return;

        this._onQueryChanged({offset: this._loadedFiles});
    }

    _onResultsChanged(parameters) {
        if (!this._awaitingResults)
            return;

        const [searchId, rows] = parameters.deep_unpack ? parameters.deep_unpack() : parameters;

        // Results can beat the Query reply back; the first signal of a request is still current.
        if (this._activeSearch === null)
            this._activeSearch = searchId;
        if (searchId !== this._activeSearch)
            return;

        const page = resultsFromRows(rows);
        this._loaded = mergeById(this._loaded, page);
        this._loadedFiles = this._loaded.filter(result => result.provider === FILE_PROVIDER).length;
        this._render(this._loaded);
    }

    _render(results) {
        // The files section is capped by the page the user asked for (contract §11, §20), never
        // below the contract's own cap, so a second page is visible rather than discarded.
        const view = buildViewModel(results, {
            caps: {files: Math.max(DEFAULT_RESULT_LIMIT, this._loadedFiles)},
        });
        const previous = this._navigator?.current ?? 0;

        // Activation is by row index, and a row index is a `flat` index. `sectionStarts` reports
        // where each section begins in that order, which is what Tab and Shift+Tab walk.
        this._results = view.flat;
        this._navigator = createNavigator(view.flat.length, {
            pageSize: DEFAULT_PAGE_SIZE,
            sections: sectionStarts(view),
            onChange: index => this._overlay?.setSelected(index),
        });
        this._overlay?.setResults(view);

        if (previous > 0)
            this._navigator.select(Math.min(previous, view.flat.length - 1));
    }

    _onKey(keyval, modifiers) {
        // Shift has to come out of the modifier mask: on Wayland Shift+Tab keeps the Tab keyval.
        const intent = intentFor(keyval, {
            pageSize: DEFAULT_PAGE_SIZE,
            shift: hasShift(modifiers),
        });
        const navigator = this._navigator;

        if (!intent || !navigator)
            return false;

        switch (intent) {
        case 'next':
            navigator.next();
            return true;
        case 'prev':
            navigator.prev();
            return true;
        case 'next-section':
            navigator.nextSection();
            return true;
        case 'prev-section':
            navigator.prevSection();
            return true;
        case 'first':
            navigator.first();
            return true;
        case 'last':
            navigator.last();
            return true;
        case 'page-up':
            navigator.pageUp(DEFAULT_PAGE_SIZE);
            return true;
        case 'page-down':
            navigator.pageDown(DEFAULT_PAGE_SIZE);
            this._requestNextPage();
            return true;
        case 'activate':
            if (navigator.current < 0)
                return false;
            this._activate(navigator.current, {modifiers});
            return true;
        case 'delete':
            return this._onDeleteKey(modifiers);
        default:
            return false;
        }
    }

    _onRowActivated(index) {
        this._navigator?.select(index);
        this._activate(index);
    }

    // Which of the row's actions a press runs is lib/activation.js's decision — plain Enter the
    // primary one, Control the secondary (`Show in Files`, or a second window for an application)
    // and Alt the copy — so only the providers the shell cannot delegate are handled here.
    _activate(index, {modifiers = 0} = {}) {
        const result = this._results[index];
        if (!result)
            return;

        if (result.provider === APPLICATION_PROVIDER)
            this._launchApplication(result, hasSecondWindowModifier(modifiers));
        else if (SHELL_PROVIDERS.has(result.provider))
            this._activateSystem(result);
        else
            this._activateWithEngine(result, activationAction(result, modifiers));
    }

    // Delete forgets (story 48): the selected clipboard entry, or — with Control, the modifier the
    // other destructive errands use — the whole history, which arms on the first press. A row that
    // does not carry the `delete` action is not something this key can forget, so it is passed on.
    _onDeleteKey(modifiers) {
        const result = this._results[this._navigator?.current ?? -1];
        if (!result?.actions?.some(action => action.id === DELETE_ACTION))
            return false;

        if (hasControlModifier(modifiers)) {
            this._clearClipboardHistory();
            return true;
        }

        this._deleteClipboardEntry(result);
        return true;
    }

    _deleteClipboardEntry(result) {
        if (this._engine.status !== EngineStatus.OK || !this._proxy) {
            this._reportFailure(`could not delete the entry: ${ENGINE_NOT_RUNNING}`);
            return;
        }

        const call = new GLib.Variant('(s)', [result.id]);
        this._proxy.call('ClipboardDelete', call, Gio.DBusCallFlags.NONE, -1, null,
            (proxy, reply) => {
                let deleted = false;
                try {
                    [deleted] = proxy.call_finish(reply).deep_unpack();
                } catch (error) {
                    logError(error, `Lumen: could not delete ${result.id}`);
                    this._reportFailure('could not delete the clipboard entry');
                    return;
                }

                if (!deleted) {
                    this._reportFailure('that clipboard entry is already gone');
                    return;
                }

                // The engine emits ClipboardChanged, which re-runs the query on screen.
                this._overlay?.setStatus('clipboard entry deleted');
            });
    }

    // Clearing has no undo, so it arms on the first press exactly like Log Out does.
    _clearClipboardHistory() {
        if (this._confirmation !== CLEAR_CLIPBOARD) {
            this._confirmation = CLEAR_CLIPBOARD;
            this._overlay?.setStatus('press Control+Delete again to clear the clipboard history');
            return;
        }
        this._confirmation = null;

        if (this._engine.status !== EngineStatus.OK || !this._proxy) {
            this._reportFailure(`could not clear the clipboard history: ${ENGINE_NOT_RUNNING}`);
            return;
        }

        this._proxy.call('ClipboardClear', null, Gio.DBusCallFlags.NONE, -1, null,
            (proxy, reply) => {
                try {
                    proxy.call_finish(reply);
                } catch (error) {
                    logError(error, 'Lumen: could not clear the clipboard history');
                    this._reportFailure('could not clear the clipboard history');
                    return;
                }

                this._overlay?.setStatus('clipboard history cleared');
            });
    }

    // Lock, suspend, log out and the setting toggles: only the shell can do these, so they never
    // reach the engine's Activate. A destructive row (log out) arms on the first press and runs on
    // the second; everything else acts immediately. A disabled row says why instead of doing
    // nothing.
    _activateSystem(result) {
        const action = result.actions?.[0] ?? null;

        if (!action) {
            this._reportFailure(result.subtitle
                ? `${result.title}: ${result.subtitle}`
                : `${result.title} is not available`);
            return;
        }

        if (!this._system) {
            this._reportFailure(`${result.title} is not available right now`);
            return;
        }

        const destructive = action.id === 'confirm' || result.preview?.destructive === true;
        if (destructive && this._confirmation !== result.id) {
            this._confirmation = result.id;
            this._overlay?.setStatus(`${result.title}: press Enter again to confirm`);
            return;
        }
        this._confirmation = null;

        const verb = actionVerbFromResultId(result.id);
        const key = settingKeyFromResultId(result.id);
        const failure = verb
            ? this._system.perform(verb)
            : key
                ? this._system.toggle(key)
                : `${result.title} is not something Lumen can run`;

        if (failure) {
            this._reportFailure(failure);
            return;
        }

        this._overlay?.close();
    }

    _launchApplication(result, newWindow) {
        const appId = appIdFromResultId(result.id);
        const app = appId ? Shell.AppSystem.get_default().lookup_app(appId) : null;

        if (!app) {
            this._reportFailure(`could not launch ${result.title}: application not found`);
            return;
        }

        // The shell launches applications itself, so the engine would never otherwise hear that one
        // was activated. Frequency is what makes the applications the user reaches for rank first
        // (ticket 13), so the activation is reported before the launch.
        this._notifyActivation(result);

        try {
            if (newWindow && app.can_open_new_window()) {
                app.open_new_window(-1);
            } else if (newWindow && app.state === Shell.AppState.RUNNING) {
                throw new Error(`${result.title} cannot open a new window`);
            } else {
                app.activate();
            }
        } catch (error) {
            logError(error, `Lumen: could not launch ${appId}`);
            this._reportFailure(`could not launch ${result.title}`);
            return;
        }

        this._overlay?.close();
    }

    // Usage learning (ticket 13): an application is launched here, never through the engine, so the
    // shell is the only half that knows it was chosen. Fire and forget — the launch has already
    // happened, and failing to report it must never be shown to the user or stop the launch.
    _notifyActivation(result) {
        if (this._engine.status !== EngineStatus.OK || !this._proxy)
            return;

        const actionId = result.actions?.[0]?.id ?? 'open';
        const call = new GLib.Variant('(ss)', [result.id, actionId]);

        this._proxy.call('Activate', call, Gio.DBusCallFlags.NONE, -1, null, (proxy, reply) => {
            try {
                proxy.call_finish(reply);
            } catch (error) {
                logError(error, `Lumen: could not record the activation of ${result.id}`);
            }
        });
    }

    _activateWithEngine(result, action = null) {
        if (this._engine.status !== EngineStatus.OK || !this._proxy) {
            this._reportFailure(`could not open ${result.title}: ${ENGINE_NOT_RUNNING}`);
            return;
        }

        const actionId = action?.id ?? result.actions?.[0]?.id ?? 'open';
        const call = new GLib.Variant('(ss)', [result.id, actionId]);

        this._proxy.call('Activate', call, Gio.DBusCallFlags.NONE, -1, null, (proxy, reply) => {
            let outcome;
            try {
                outcome = unpack(proxy.call_finish(reply).deep_unpack()[0] ?? {});
            } catch (error) {
                logError(error, `Lumen: could not activate ${result.id}`);
                this._reportFailure(`could not open ${result.title}`);
                return;
            }

            this._onActivated(result, outcome);
        });
    }

    // `handled: true` means the daemon did it (it opens files and recent documents itself). A shell
    // action is the shell's: the browser for `open-url`, the clipboard for `copy`, the file manager
    // for `reveal`. Anything else is a row with nothing to run, and says so rather than looking
    // like a no-op.
    _onActivated(result, outcome) {
        if (outcome.handled) {
            this._overlay?.close();
            return;
        }

        switch (outcome.shell_action) {
        case 'open-url':
            this._openUrl(result, outcome.args ?? []);
            return;
        case 'copy':
            this._copyToClipboard(result, outcome.args ?? []);
            return;
        case 'reveal':
            this._revealInFiles(result, outcome.args ?? []);
            return;
        default:
            this._reportFailure(`${result.title} has nothing to run`);
        }
    }

    _openUrl(result, args) {
        const [url] = args;
        if (!url) {
            this._reportFailure(`could not open ${result.title}: no url`);
            return;
        }

        try {
            Gio.AppInfo.launch_default_for_uri(url, null);
        } catch (error) {
            logError(error, `Lumen: could not open ${url}`);
            this._reportFailure('could not open the web search');
            return;
        }

        this._overlay?.close();
    }

    // Only the shell owns the Wayland clipboard (contract §2), so the daemon hands the text over
    // and writing it is ours. A write that fails is left on the status label, never swallowed.
    _copyToClipboard(result, args) {
        const [text] = args;
        if (!text) {
            this._reportFailure(`could not copy ${result.title}: nothing to copy`);
            return;
        }

        try {
            this._clipboard.set_text(St.ClipboardType.CLIPBOARD, text);
        } catch (error) {
            logError(error, `Lumen: could not copy ${result.title}`);
            this._reportFailure(`could not copy ${result.title}`);
            return;
        }

        this._overlay?.close();
    }

    // `Show in Files` goes through the interface browsers use to reveal a download
    // (`org.freedesktop.FileManager1`, implemented by Nautilus); a session without it opens the
    // containing folder instead, and either failure ends up on the status label.
    _revealInFiles(result, args) {
        const [path] = args;
        if (!path) {
            this._reportFailure(`could not show ${result.title}: no path`);
            return;
        }

        let uri;
        try {
            uri = GLib.filename_to_uri(path, null);
        } catch (error) {
            logError(error, `Lumen: ${path} is not a path Lumen can show`);
            this._reportFailure(`could not show ${result.title} in Files`);
            return;
        }

        const bus = Gio.DBus.session;
        if (!bus) {
            this._openContainingFolder(result, path);
            return;
        }

        bus.call(
            'org.freedesktop.FileManager1',
            '/org/freedesktop/FileManager1',
            'org.freedesktop.FileManager1',
            'ShowItems',
            new GLib.Variant('(ass)', [[uri], '']),
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null,
            (connection, reply) => {
                try {
                    connection.call_finish(reply);
                } catch (error) {
                    logError(error, `Lumen: the file manager did not show ${path}`);
                    this._openContainingFolder(result, path);
                    return;
                }

                this._overlay?.close();
            });
    }

    _openContainingFolder(result, path) {
        const folder = GLib.path_get_dirname(path);

        try {
            Gio.AppInfo.launch_default_for_uri(GLib.filename_to_uri(folder, null), null);
        } catch (error) {
            logError(error, `Lumen: could not open ${folder}`);
            this._reportFailure(`could not show ${result.title} in Files`);
            return;
        }

        this._overlay?.close();
    }

    // A failed launch must not look like a no-op, so the reason is left on screen.
    _reportFailure(message) {
        log(`Lumen: ${message}`);
        this._overlay?.setStatus(message);
    }

    _applicationCandidates() {
        if (this._apps)
            return this._apps;

        const system = Shell.AppSystem.get_default();
        const seen = new Set();
        const apps = [];

        for (const app of [...system.get_running(), ...system.get_installed()]) {
            const id = app.get_id();
            if (!id || seen.has(id))
                continue;

            seen.add(id);
            apps.push({
                id,
                name: app.get_name() ?? '',
                description: app.get_description() ?? '',
                icon: iconName(app),
            });
        }

        this._apps = apps;
        return apps;
    }
}

function iconName(app) {
    const icon = app.get_icon();
    return icon instanceof Gio.ThemedIcon ? (icon.get_names()[0] ?? '') : '';
}

function variantsOf(results) {
    return results.map(result => ({
        id: new GLib.Variant('s', result.id),
        provider: new GLib.Variant('s', result.provider),
        title: new GLib.Variant('s', result.title),
        subtitle: new GLib.Variant('s', result.subtitle),
        icon: new GLib.Variant('s', result.icon),
        score: new GLib.Variant('d', result.score),
        actions: new GLib.Variant('aa{sv}', result.actions.map(action => ({
            id: new GLib.Variant('s', action.id),
            label: new GLib.Variant('s', action.label),
            handler: new GLib.Variant('s', action.handler),
        }))),
        preview: new GLib.Variant('a{sv}', variantsOfFields(result.preview)),
    }));
}

function variantsOfFields(fields) {
    const variant = {};
    for (const [key, value] of Object.entries(fields ?? {})) {
        // Preview fields are strings (§6); the flags the actions and toggles add (destructive,
        // disabled) are booleans and stay booleans across the daemon round trip.
        variant[key] = typeof value === 'boolean'
            ? new GLib.Variant('b', value)
            : new GLib.Variant('s', String(value ?? ''));
    }
    return variant;
}

// A page of results joined to the ones already loaded, by result id: the daemon answers a later
// page with the same ranking applied to a later slice, so the same row can arrive twice. The
// viewmodel sorts what it is given, so the order here only has to be stable.
function mergeById(loaded, page) {
    const seen = new Set((loaded ?? []).map(result => result.id));
    return [...(loaded ?? []), ...(page ?? []).filter(result => !seen.has(result.id))];
}
