import Gio from 'gi://Gio';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {LumenOverlay} from './overlay.js';
import {engineState} from './lib/engine.js';
import {BUS_NAME, OBJECT_PATH, INTERFACE, SUPPORTED_MAJOR} from './lib/protocol.js';

const SUMMON_KEYBINDING = 'summon-shortcut';

export default class LumenExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._shutdown = false;
        this._proxy = null;
        this._proxySignals = [];
        this._owner = null;
        this._keybindingBound = false;
        this._engineStatus = null;

        this._overlay = new LumenOverlay();
        this._setEngineState(null, null);

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
        this._overlay?.destroy();
        this._overlay = null;
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
        this._proxySignals = [
            proxy.connect('g-properties-changed', () => this._readVersion()),
            proxy.connect('notify::g-name-owner', () => this._readVersion()),
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
        if (state.status !== this._engineStatus) {
            this._engineStatus = state.status;
            log(`Lumen: engine ${state.status} (${state.message})`);
        }
        this._overlay?.setEngineState(state);
    }

    _dropProxy() {
        const proxy = this._proxy;
        this._proxy = null;

        for (const id of this._proxySignals)
            proxy?.disconnect(id);
        this._proxySignals = [];
    }
}
