// The summon overlay: a full-screen transparent backdrop with a translucent, rounded card in
// front of it. Everything here is shell-only (St/Clutter/Main); the testable model lives in
// lib/. Ticket 03 grows a results list and preview inside `card`; ticket 15 moves the placement
// from the primary monitor to the pointer's monitor.

import Clutter from 'gi://Clutter';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {ENGINE_NOT_RUNNING} from './lib/engine.js';

const CARD_MAX_WIDTH = 640;
const MONITOR_MARGIN = 80;

export class LumenOverlay {
    constructor({onClose = null} = {}) {
        this._onClose = onClose;
        this._isOpen = false;
        this._grab = null;
        this._keyFocusId = 0;
        this._capturedEventId = 0;
        this._monitorsChangedId = 0;
        this._buttonPressId = 0;

        this._card = new St.BoxLayout({
            style_class: 'lumen-card',
            vertical: true,
            reactive: true,
            spacing: 4,
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.CENTER,
        });

        this._entry = new St.Entry({
            style_class: 'lumen-entry',
            x_expand: true,
            can_focus: true,
            hint_text: 'Search…',
        });

        this._status = new St.Label({
            style_class: 'lumen-status',
            x_expand: true,
            text: ENGINE_NOT_RUNNING,
        });

        this._card.add_child(this._entry);
        this._card.add_child(this._status);

        this._backdrop = new St.Widget({
            style_class: 'lumen-backdrop',
            reactive: true,
            visible: false,
            layout_manager: new Clutter.BinLayout(),
        });
        this._backdrop.add_child(this._card);

        this._buttonPressId = this._backdrop.connect(
            'button-press-event', (...args) => this._onButtonPress(...args));
        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed', () => this._syncGeometry());

        Main.layoutManager.uiGroup.add_child(this._backdrop);
    }

    get isOpen() {
        return this._isOpen;
    }

    /** The St.Entry holding the query. Ticket 03 connects to its clutter_text. */
    get entry() {
        return this._entry;
    }

    /** The card the result list and preview will be added to. */
    get card() {
        return this._card;
    }

    open() {
        if (this._isOpen)
            return;

        this._isOpen = true;
        this._syncGeometry();
        this._entry.set_text('');
        this._backdrop.show();

        this._grab = Main.pushModal(this._backdrop) ?? null;
        this._entry.grab_key_focus();

        this._capturedEventId = global.stage.connect(
            'captured-event', (...args) => this._onCapturedEvent(...args));
        this._keyFocusId = global.stage.connect(
            'notify::key-focus', () => this._onKeyFocusChanged());
    }

    close() {
        if (!this._isOpen)
            return;

        this._isOpen = false;

        if (this._capturedEventId) {
            global.stage.disconnect(this._capturedEventId);
            this._capturedEventId = 0;
        }

        if (this._keyFocusId) {
            global.stage.disconnect(this._keyFocusId);
            this._keyFocusId = 0;
        }

        if (this._grab) {
            Main.popModal(this._grab);
            this._grab = null;
        }

        this._backdrop.hide();

        if (this._onClose)
            this._onClose();
    }

    /** Show the daemon handshake state (`{status, message}` from lib/engine.js). */
    setEngineState(state) {
        this._status.set_text(state?.message ?? ENGINE_NOT_RUNNING);
    }

    destroy() {
        this.close();

        if (this._monitorsChangedId) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }

        if (this._buttonPressId) {
            this._backdrop.disconnect(this._buttonPressId);
            this._buttonPressId = 0;
        }

        this._backdrop.get_parent()?.remove_child(this._backdrop);
        this._backdrop.destroy();
        this._backdrop = null;
        this._card = null;
        this._entry = null;
        this._status = null;
        this._onClose = null;
    }

    // Ticket 15 replaces this with the monitor under the pointer.
    _syncGeometry() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor)
            return;

        this._backdrop.set_position(monitor.x, monitor.y);
        this._backdrop.set_size(monitor.width, monitor.height);
        this._card.set_width(
            Math.min(CARD_MAX_WIDTH, Math.max(320, monitor.width - MONITOR_MARGIN)));
    }

    _onCapturedEvent(_actor, event) {
        if (event.type() !== Clutter.EventType.KEY_PRESS)
            return Clutter.EVENT_PROPAGATE;

        if (event.get_key_symbol() === Clutter.KEY_Escape) {
            this.close();
            return Clutter.EVENT_STOP;
        }

        return Clutter.EVENT_PROPAGATE;
    }

    _onButtonPress(_actor, event) {
        if (!this._owns(event.get_source?.() ?? null)) {
            this.close();
            return Clutter.EVENT_STOP;
        }

        return Clutter.EVENT_PROPAGATE;
    }

    // Losing the grab (another modal, lock screen, …) must close us, not leave a dead overlay.
    _onKeyFocusChanged() {
        if (!this._isOpen)
            return;

        const focus = global.stage.get_key_focus();
        if (focus && !this._owns(focus))
            this.close();
    }

    _owns(actor) {
        for (let ancestor = actor; ancestor; ancestor = ancestor.get_parent()) {
            if (ancestor === this._backdrop)
                return true;
        }
        return false;
    }
}
