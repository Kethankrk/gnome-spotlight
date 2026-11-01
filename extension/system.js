// The shell half of the system actions and setting toggles: read the live state the candidates
// need, keep it fresh by watching the signals of its sources, and perform an action or flip a
// toggle when the overlay asks.
//
// Shell-only (Gio/GLib/Main/SystemActions) — the wording, matching and the confirmation rule live
// in `lib/actions.js`, which node tests. Every source degrades to a clearly unavailable candidate
// instead of throwing: NetworkManager or bluetoothd can be missing, absent or restarted at any
// time, and the shell can be running without a system bus at all.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as SystemActions from 'resource:///org/gnome/shell/misc/systemActions.js';

const NM_BUS_NAME = 'org.freedesktop.NetworkManager';
const NM_PATH = '/org/freedesktop/NetworkManager';
const NM_INTERFACE = 'org.freedesktop.NetworkManager';
const BLUEZ_BUS_NAME = 'org.bluez';
const BLUEZ_ROOT_PATH = '/';
const BLUEZ_OBJECT_MANAGER = 'org.bluez.ObjectManager';
const BLUEZ_ADAPTER = 'org.bluez.Adapter1';
const PROPERTIES_INTERFACE = 'org.freedesktop.DBus.Properties';
const LOGIN1_BUS_NAME = 'org.freedesktop.login1';
const LOGIN1_PATH = '/org/freedesktop/login1';
const LOGIN1_INTERFACE = 'org.freedesktop.login1.Manager';
const SESSION_MANAGER_BUS_NAME = 'org.gnome.SessionManager';
const SESSION_MANAGER_PATH = '/org/gnome/SessionManager';
const SESSION_MANAGER_INTERFACE = 'org.gnome.SessionManager';
const SCREEN_SAVER_BUS_NAME = 'org.gnome.ScreenSaver';
const SCREEN_SAVER_PATH = '/org/gnome/ScreenSaver';
const SCREEN_SAVER_INTERFACE = 'org.gnome.ScreenSaver';

const NOTIFICATIONS_SCHEMA = 'org.gnome.desktop.notifications';
const INTERFACE_SCHEMA = 'org.gnome.desktop.interface';
const LOCKDOWN_SCHEMA = 'org.gnome.desktop.lockdown';
const SHOW_BANNERS_KEY = 'show-banners';
const COLOR_SCHEME_KEY = 'color-scheme';
const DARK_SCHEME = 'prefer-dark';
const DEFAULT_SCHEME = 'default';
const DISABLE_LOG_OUT_KEY = 'disable-log-out';

const DBUS_TIMEOUT_MS = 1500;

/**
 * The live system state behind the `actions` and `settings` providers.
 *
 * `actions` and `toggles` hand plain objects to `lib/actions.js`; `perform(verb)` and `toggle(key)`
 * return `null` when the work is dispatched, or a message to show the user. `onChanged` fires when
 * a source reports new state, so the overlay can re-query with it.
 */
export class LumenSystem {
    constructor({onChanged = null, onError = null} = {}) {
        this._onChanged = onChanged;
        this._onError = onError;
        this._destroyed = false;
        this._watched = [];
        this._settings = [];
        this._adapter = null;
        this._adapterPath = null;

        // `SystemActions.getDefault()` is what the shell's own power menu and lock button use: it
        // knows which of lock, suspend and log out this session may actually do.
        this._systemActions = defaultSystemActions();

        this._notifications = this._settingsFor(NOTIFICATIONS_SCHEMA);
        this._interface = this._settingsFor(INTERFACE_SCHEMA);
        this._lockdown = this._settingsFor(LOCKDOWN_SCHEMA);

        for (const [settings, key] of [
            [this._notifications, SHOW_BANNERS_KEY],
            [this._interface, COLOR_SCHEME_KEY],
        ]) {
            if (settings)
                this._watch(settings, `changed::${key}`, () => this._changed());
        }

        // The shell recomputes availability asynchronously (polkit, session mode, lockdown), so
        // these can flip under us while the overlay is open.
        for (const property of ['can-lock-screen', 'can-suspend', 'can-logout']) {
            if (this._systemActions)
                this._watch(this._systemActions, `notify::${property}`, () => this._changed());
        }

        this._wifi = new ProxySource({
            bus: 'system',
            name: NM_BUS_NAME,
            path: NM_PATH,
            iface: NM_INTERFACE,
            onChanged: () => this._changed(),
        });

        this._bluez = new ProxySource({
            bus: 'system',
            name: BLUEZ_BUS_NAME,
            path: BLUEZ_ROOT_PATH,
            iface: BLUEZ_OBJECT_MANAGER,
            onChanged: () => this._changed(),
            // bluetoothd is usually already running when Lumen starts, so discovery is driven by the
            // proxy becoming ready as well as by the name appearing or an adapter being added.
            onReady: () => this._findAdapter(),
        });

        // bluetoothd can start, stop or swap its adapter while we are watching.
        this._watch(this._bluez.proxy, 'g-signal', (_proxy, _sender, signal) => {
            if (signal === 'InterfacesAdded' || signal === 'InterfacesRemoved')
                this._findAdapter();
        });
        this._watch(this._bluez.proxy, 'notify::g-name-owner', () => {
            if (this._bluez.proxy.g_name_owner)
                this._findAdapter();
            else
                this._useAdapter(null);
        });
    }

    /** `[{verb, available, reason}]` for lock, suspend and log out. */
    get actions() {
        const actions = this._systemActions;

        return [
            {
                verb: 'lock',
                available: actions ? actions.can_lock_screen : true,
                reason: 'locking the screen is disabled in this session',
            },
            {
                verb: 'suspend',
                available: actions ? actions.can_suspend : true,
                reason: 'this session cannot suspend',
            },
            {
                verb: 'logout',
                available: this._lockdown?.get_boolean(DISABLE_LOG_OUT_KEY) !== true,
                reason: 'logging out is disabled by policy',
            },
        ];
    }

    /** `[{key, on, available, reason, detail}]` for Wi-Fi, Bluetooth, Do Not Disturb and appearance. */
    get toggles() {
        return [this._wifiState(), this._bluetoothState(), this._dndState(), this._appearanceState()];
    }

    /** Perform `lock`, `suspend` or `logout`. Returns null on dispatch, or a message. */
    perform(verb) {
        if (this._destroyed)
            return 'Lumen is shutting down';

        try {
            switch (verb) {
            case 'lock':
                return this._lock();
            case 'suspend':
                return this._suspend();
            case 'logout':
                return this._logout();
            default:
                return `there is nothing to run for ${verb}`;
            }
        } catch (error) {
            logError(error, `Lumen: could not ${verb}`);
            return `could not ${verb}: ${error.message}`;
        }
    }

    /** Flip the switch behind `key`, whatever it currently is. Returns null, or a message. */
    toggle(key) {
        if (this._destroyed)
            return 'Lumen is shutting down';

        try {
            switch (key) {
            case 'wifi':
                return this._toggleWifi();
            case 'bluetooth':
                return this._toggleBluetooth();
            case 'dnd':
                return this._toggleDnd();
            case 'appearance':
                return this._toggleAppearance();
            default:
                return `there is no ${key} toggle`;
            }
        } catch (error) {
            logError(error, `Lumen: could not toggle ${key}`);
            return `could not toggle ${key}: ${error.message}`;
        }
    }

    destroy() {
        this._destroyed = true;

        for (const {object, id} of this._watched)
            object.disconnect(id);
        this._watched = [];

        this._adapter?.destroy();
        this._adapter = null;
        this._wifi?.destroy();
        this._wifi = null;
        this._bluez?.destroy();
        this._bluez = null;

        for (const settings of this._settings)
            settings.run_dispose();
        this._settings = [];
        this._notifications = null;
        this._interface = null;
        this._lockdown = null;
        this._systemActions = null;
        this._onChanged = null;
        this._onError = null;
    }

    // --- the three actions ---------------------------------------------------

    _lock() {
        if (this._systemActions?.can_lock_screen) {
            this._systemActions.activateLockScreen();
            return null;
        }

        if (Main.screenShield) {
            Main.screenShield.lock(true);
            return null;
        }

        return this._call({
            bus: 'session',
            name: SCREEN_SAVER_BUS_NAME,
            path: SCREEN_SAVER_PATH,
            iface: SCREEN_SAVER_INTERFACE,
            method: 'Lock',
            what: 'lock the screen',
        });
    }

    _suspend() {
        if (this._systemActions?.can_suspend) {
            this._systemActions.activateSuspend();
            return null;
        }

        return this._call({
            bus: 'system',
            name: LOGIN1_BUS_NAME,
            path: LOGIN1_PATH,
            iface: LOGIN1_INTERFACE,
            method: 'Suspend',
            parameters: new GLib.Variant('(b)', [true]),
            what: 'suspend the computer',
        });
    }

    // Log out goes through the session manager the same way the shell's own menu item does; mode 0
    // means gnome-session asks the user to confirm before it ends the session.
    _logout() {
        if (this._systemActions?.can_logout) {
            this._systemActions.activateLogout();
            return null;
        }

        return this._call({
            bus: 'session',
            name: SESSION_MANAGER_BUS_NAME,
            path: SESSION_MANAGER_PATH,
            iface: SESSION_MANAGER_INTERFACE,
            method: 'Logout',
            parameters: new GLib.Variant('(u)', [0]),
            what: 'log out',
        });
    }

    // --- the four toggles ----------------------------------------------------

    _wifiState() {
        const state = {key: 'wifi', on: false, available: true, reason: '', detail: ''};

        if (!this._wifi?.serving)
            return {...state, available: false, reason: 'NetworkManager is not running'};

        return {
            ...state,
            on: this._wifi.get('WirelessEnabled') === true,
            detail: this._wifi.get('WirelessHardwareEnabled') === false
                ? 'the hardware switch is off'
                : '',
        };
    }

    _bluetoothState() {
        const state = {key: 'bluetooth', on: false, available: true, reason: '', detail: ''};

        if (!this._bluez?.serving)
            return {...state, available: false, reason: 'the Bluetooth service is not running'};

        if (!this._adapter)
            return {...state, available: false, reason: 'there is no Bluetooth adapter'};

        return {...state, on: this._adapter.get('Powered') === true};
    }

    _dndState() {
        if (!this._notifications)
            return {key: 'dnd', on: false, available: false, reason: 'notifications are not configurable', detail: ''};

        return {
            key: 'dnd',
            on: !this._notifications.get_boolean(SHOW_BANNERS_KEY),
            available: true,
            reason: '',
            detail: '',
        };
    }

    _appearanceState() {
        if (!this._interface)
            return {key: 'appearance', on: false, available: false, reason: 'the theme is not configurable', detail: ''};

        return {
            key: 'appearance',
            on: this._interface.get_string(COLOR_SCHEME_KEY) === DARK_SCHEME,
            available: true,
            reason: '',
            detail: '',
        };
    }

    _toggleWifi() {
        if (!this._wifi?.serving)
            return 'Wi-Fi is not available: NetworkManager is not running';

        return this._setProperty(NM_BUS_NAME, NM_PATH, NM_INTERFACE, 'WirelessEnabled',
            new GLib.Variant('b', this._wifi.get('WirelessEnabled') !== true), 'system', 'Wi-Fi');
    }

    _toggleBluetooth() {
        if (!this._bluez?.serving)
            return 'Bluetooth is not available: the Bluetooth service is not running';

        if (!this._adapter)
            return 'Bluetooth is not available: there is no Bluetooth adapter';

        return this._setProperty(BLUEZ_BUS_NAME, this._adapterPath, BLUEZ_ADAPTER, 'Powered',
            new GLib.Variant('b', this._adapter.get('Powered') !== true), 'system', 'Bluetooth');
    }

    _toggleDnd() {
        if (!this._notifications)
            return 'notifications are not configurable';

        const on = this._notifications.get_boolean(SHOW_BANNERS_KEY);
        this._notifications.set_boolean(SHOW_BANNERS_KEY, !on);
        return null;
    }

    _toggleAppearance() {
        if (!this._interface)
            return 'the theme is not configurable';

        const dark = this._interface.get_string(COLOR_SCHEME_KEY) === DARK_SCHEME;
        this._interface.set_string(COLOR_SCHEME_KEY, dark ? DEFAULT_SCHEME : DARK_SCHEME);

        // The shell animates the same change from its own dark-mode toggle.
        try {
            Main.layoutManager.screenTransition.run();
        } catch {
            // No transition available: the setting still changed.
        }

        return null;
    }

    // --- bluetooth adapter discovery -----------------------------------------

    // bluez numbers its adapters (`hci0`, `hci1`, …), so ask its object manager instead of guessing.
    _findAdapter() {
        const proxy = this._bluez?.proxy;

        if (!proxy?.g_name_owner) {
            this._useAdapter(null);
            return;
        }

        // Not initialised yet: the proxy will call us again through `onReady`.
        if (!this._bluez.ready)
            return;

        proxy.call('GetManagedObjects', null, Gio.DBusCallFlags.NONE, DBUS_TIMEOUT_MS, null,
            (source, result) => {
                let objects;
                try {
                    [objects] = source.call_finish(result).deep_unpack();
                } catch (error) {
                    logError(error, 'Lumen: could not list the Bluetooth adapters');
                    this._useAdapter(null);
                    return;
                }

                const path = Object.keys(objects ?? {})
                    .find(candidate => isAdapter(objects[candidate]));
                this._useAdapter(path ?? null);
            });
    }

    _useAdapter(path) {
        // Devices coming and going also raise interfaces, and must not re-run the search.
        if (this._adapterPath === path)
            return;

        this._adapterPath = path;
        this._adapter?.destroy();
        this._adapter = path
            ? new ProxySource({
                bus: 'system',
                name: BLUEZ_BUS_NAME,
                path,
                iface: BLUEZ_ADAPTER,
                onChanged: () => this._changed(),
            })
            : null;

        this._changed();
    }

    // --- D-Bus plumbing ------------------------------------------------------

    // Fire and forget: a failure lands on the status label through `onError` rather than blocking
    // the shell on a call that may never answer.
    _call({bus, name, path, iface, method, parameters = null, what}) {
        const connection = this._connection(bus);
        if (!connection)
            return `could not ${what}: there is no ${bus} bus`;

        connection.call(name, path, iface, method, parameters, null, Gio.DBusCallFlags.NONE,
            DBUS_TIMEOUT_MS, null, (source, result) => {
                try {
                    source.call_finish(result);
                } catch (error) {
                    logError(error, `Lumen: could not ${what}`);
                    this._report(`could not ${what}: ${error.message}`);
                }
            });

        return null;
    }

    _setProperty(name, path, iface, property, value, bus, what) {
        return this._call({
            bus,
            name,
            path,
            iface: PROPERTIES_INTERFACE,
            method: 'Set',
            parameters: new GLib.Variant('(ssv)', [iface, property, value]),
            what: `change ${what}`,
        });
    }

    _connection(bus) {
        return bus === 'system' ? (Gio.DBus.system ?? null) : (Gio.DBus.session ?? null);
    }

    _settingsFor(schemaId) {
        try {
            const settings = new Gio.Settings({schema_id: schemaId});
            this._settings.push(settings);
            return settings;
        } catch (error) {
            logError(error, `Lumen: no ${schemaId} settings`);
            return null;
        }
    }

    _watch(object, signal, handler) {
        // A source that is not a signal emitter (a stub, a shell build without SystemActions) must
        // not take the whole extension down with it.
        if (!object || typeof object.connect !== 'function')
            return;

        this._watched.push({object, id: object.connect(signal, handler)});
    }

    _changed() {
        if (!this._destroyed)
            this._onChanged?.();
    }

    _report(message) {
        this._onError?.(message);
    }
}

// One watched D-Bus proxy: ready once it is initialised, serving once its name has an owner, and
// re-initialised when a service appears on the bus after us.
class ProxySource {
    constructor({bus, name, path, iface, onChanged = null, onReady = null}) {
        this._onChanged = onChanged;
        this._onReady = onReady;
        this._ready = false;
        this._initialising = false;
        this._watched = [];
        this.proxy = null;

        const connection = bus === 'system' ? (Gio.DBus.system ?? null) : (Gio.DBus.session ?? null);
        if (!connection)
            return;

        this.proxy = new Gio.DBusProxy({
            g_connection: connection,
            g_name: name,
            g_object_path: path,
            g_interface_name: iface,
        });

        this._watch('g-properties-changed', () => this._changed());
        this._watch('notify::g-name-owner', () => this._sync());
        this._start();
    }

    /** The proxy is initialised: its cached properties can be read. */
    get ready() {
        return this._ready;
    }

    /** The proxy is initialised and the name it watches is on the bus. */
    get serving() {
        return this._ready && this.proxy?.g_name_owner != null;
    }

    get(property) {
        return this.proxy?.get_cached_property(property)?.deep_unpack() ?? undefined;
    }

    destroy() {
        for (const {signal, id} of this._watched)
            this.proxy?.disconnect(id);

        this._watched = [];
        this.proxy = null;
        this._ready = false;
        this._onChanged = null;
        this._onReady = null;
    }

    _start() {
        if (this._initialising || !this.proxy)
            return;

        this._initialising = true;
        this.proxy.init_async(GLib.PRIORITY_DEFAULT, null).then(() => {
            this._initialising = false;
            this._ready = true;
            this._onReady?.();
            this._changed();
        }).catch(error => {
            this._initialising = false;
            this._ready = false;
            logError(error, `Lumen: could not talk to ${this.proxy?.g_name}`);
            this._changed();
        });
    }

    // A service that starts after us leaves the proxy uninitialised; init again now that it is there.
    _sync() {
        if (!this._ready)
            this._start();

        this._changed();
    }

    _watch(signal, handler) {
        this._watched.push({signal, id: this.proxy.connect(signal, handler)});
    }

    _changed() {
        this._onChanged?.();
    }
}

function defaultSystemActions() {
    try {
        return SystemActions.getDefault();
    } catch (error) {
        logError(error, 'Lumen: the shell exposes no system actions; using D-Bus instead');
        return null;
    }
}

function isAdapter(interfaces) {
    return interfaces != null && Object.prototype.hasOwnProperty.call(interfaces, BLUEZ_ADAPTER);
}
