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
import {BUS_NAME, OBJECT_PATH, INTERFACE, SUPPORTED_MAJOR} from './lib/protocol.js';
import {buildRequest, hasSecondWindowModifier} from './lib/query.js';
import {buildViewModel} from './lib/viewmodel.js';
import {createNavigator} from './lib/navigation.js';
import {DEFAULT_PAGE_SIZE, intentFor} from './lib/keys.js';
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
const APPLICATION_PROVIDER = 'applications';
// Providers the shell resolves itself: the daemon ranks them, but only the shell can run them.
const SHELL_PROVIDERS = new Set([ACTION_PROVIDER, SETTINGS_PROVIDER]);

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
        this._navigator = null;
        this._apps = null;
        this._clipboard = St.Clipboard.get_default();
        this._sampler = createClipboardSampler();
        this._captureTimerId = 0;
        this._confirmation = null;

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
        this._settings = null;

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
        this._apps = null;
        this._clipboard = null;
        this._sampler = null;
        this._confirmation = null;
    }

    _bindShortcut() {
        if (this._keybindingBound) {
            Main.wm.removeKeybinding(SUMMON_KEYBINDING);
            this._keybindingBound = false;
        }

        Main.wm.addKeybinding(
            SUMMON_KEYBINDING,
            this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.ALL,
            () => this._toggleOverlay());
        this._keybindingBound = true;
    }

    _toggleOverlay() {
        if (this._overlay.isOpen) {
            this._overlay.close();
            return;
        }

        if (Main.modalCount > 0)
            return;

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

        if (state.status !== EngineStatus.OK)
            this._render([]);
        else if (changed && this._overlay?.entry.get_text())
            this._onQueryChanged();
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
    // on a timer while — and only while — the engine says history is on.
    _clipboardEnabled() {
        return this._proxy?.get_cached_property('ClipboardEnabled')?.deep_unpack() === true;
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

    // Wi-Fi, Bluetooth, Do Not Disturb, the theme, or a lock/suspend availability policy changed
    // outside the overlay: search again so the rows show the new state.
    _onSystemStateChanged() {
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
    _onQueryChanged() {
        this._activeSearch = null;
        this._awaitingResults = false;
        this._confirmation = null;

        const request = buildRequest(
            this._overlay?.entry.get_text() ?? '',
            this._settings?.get_strv(PROVIDERS_KEY) ?? [],
        );

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

        const call = new GLib.Variant(
            '(sasa{sv})',
            [request.text, request.enabled, {injected: variantsOf(injected)}],
        );

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

    _onResultsChanged(parameters) {
        if (!this._awaitingResults)
            return;

        const [searchId, rows] = parameters.deep_unpack ? parameters.deep_unpack() : parameters;

        // Results can beat the Query reply back; the first signal of a request is still current.
        if (this._activeSearch === null)
            this._activeSearch = searchId;
        if (searchId !== this._activeSearch)
            return;

        this._render(resultsFromRows(rows));
    }

    _render(results) {
        const view = buildViewModel(results);
        const previous = this._navigator?.current ?? 0;

        // Activation is by row index, and a row index is a `flat` index.
        this._results = view.flat;
        this._navigator = createNavigator(view.flat.length, {
            pageSize: DEFAULT_PAGE_SIZE,
            onChange: index => this._overlay?.setSelected(index),
        });
        this._overlay?.setResults(view);

        if (previous > 0)
            this._navigator.select(Math.min(previous, view.flat.length - 1));
    }

    _onKey(keyval, modifiers) {
        const intent = intentFor(keyval, {pageSize: DEFAULT_PAGE_SIZE});
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
            return true;
        case 'activate':
            if (navigator.current < 0)
                return false;
            this._activate(navigator.current, {newWindow: hasSecondWindowModifier(modifiers)});
            return true;
        default:
            return false;
        }
    }

    _onRowActivated(index) {
        this._navigator?.select(index);
        this._activate(index, {newWindow: false});
    }

    _activate(index, {newWindow = false} = {}) {
        const result = this._results[index];
        if (!result)
            return;

        if (result.provider === APPLICATION_PROVIDER)
            this._launchApplication(result, newWindow);
        else if (SHELL_PROVIDERS.has(result.provider))
            this._activateSystem(result);
        else
            this._activateWithEngine(result);
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

    _activateWithEngine(result) {
        if (this._engine.status !== EngineStatus.OK || !this._proxy) {
            this._reportFailure(`could not open ${result.title}: ${ENGINE_NOT_RUNNING}`);
            return;
        }

        const actionId = result.actions?.[0]?.id ?? 'open';
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

    _onActivated(result, outcome) {
        if (outcome.handled) {
            this._overlay?.close();
            return;
        }

        if (outcome.shell_action === 'open-url') {
            const [url] = outcome.args ?? [];
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
            return;
        }

        this._reportFailure(`${result.title} has nothing to run`);
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
