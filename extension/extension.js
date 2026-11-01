import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

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
        this._confirmation = null;

        this._overlay = new LumenOverlay({
            onClose: () => this._onOverlayClosed(),
            onActivate: index => this._onRowActivated(index),
            onKey: (keyval, modifiers) => this._onKey(keyval, modifiers),
        });
        this._setEngineState(null, null);

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
