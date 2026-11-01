import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

// Ticket 01: the extension must load and disable cleanly and hold nothing across disable().
// The overlay, keybinding and D-Bus client arrive in later tickets.
export default class LumenExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
    }

    disable() {
        this._settings = null;
    }
}
