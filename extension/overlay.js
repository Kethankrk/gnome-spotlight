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
const ROW_ICON_SIZE = 24;
const FALLBACK_ICON = 'application-x-executable';

export class LumenOverlay {
    constructor({onClose = null, onActivate = null, onKey = null} = {}) {
        this._onClose = onClose;
        this._onActivate = onActivate;
        this._onKey = onKey;
        this._isOpen = false;
        this._grab = null;
        this._keyFocusId = 0;
        this._capturedEventId = 0;
        this._monitorsChangedId = 0;
        this._buttonPressId = 0;
        this._rows = [];
        this._selectedRow = null;

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

        this._results = new St.BoxLayout({
            style_class: 'lumen-results',
            vertical: true,
            x_expand: true,
        });

        this._scroll = new St.ScrollView({
            style_class: 'lumen-scroll',
            x_expand: true,
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.AUTOMATIC,
            child: this._results,
        });

        this._card.add_child(this._entry);
        this._card.add_child(this._status);
        this._card.add_child(this._scroll);


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
        this.setStatus(state?.message ?? ENGINE_NOT_RUNNING);
    }

    /** The one line under the entry: engine state, or why an activation failed. */
    setStatus(text) {
        this._status.set_text(text ?? '');
    }

    /**
     * Render a view model from lib/viewmodel.js. Row indices are the `flat` model order: the Top
     * Hit first, then each section. Clicking a row calls `onActivate(index)`.
     */
    setResults({topHit = null, sections = []} = {}) {
        this._clearRows();

        if (topHit) {
            const group = new St.BoxLayout({
                style_class: 'lumen-tophit',
                vertical: true,
                x_expand: true,
            });
            group.add_child(this._createRow(topHit, 0));
            this._results.add_child(group);
        }

        let index = topHit ? 1 : 0;
        for (const section of sections) {
            this._results.add_child(new St.Label({
                style_class: 'lumen-section-header',
                text: section.label ?? section.provider ?? '',
                x_align: Clutter.ActorAlign.START,
            }));
            for (const item of section.items ?? [])
                this._results.add_child(this._createRow(item, index++));
        }

        this.setSelected(this._rows.length > 0 ? 0 : -1);
    }

    /** Highlight row `index` (a `flat` index) and scroll it into view. */
    setSelected(index) {
        this._selectedRow?.remove_style_class_name('lumen-row-selected');
        this._selectedRow = null;

        const row = this._rows[index] ?? null;
        if (!row)
            return;

        row.add_style_class_name('lumen-row-selected');
        this._selectedRow = row;
        this._scrollIntoView(row);
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
        this._scroll = null;
        this._results = null;
        this._rows = [];
        this._selectedRow = null;
        this._onClose = null;
        this._onActivate = null;
        this._onKey = null;
    }

    _clearRows() {
        for (const child of this._results.get_children())
            child.destroy();

        this._rows = [];
        this._selectedRow = null;
    }

    _createRow(result, index) {
        const row = new St.BoxLayout({
            style_class: 'lumen-row',
            vertical: false,
            x_expand: true,
            reactive: true,
            track_hover: true,
            spacing: 10,
        });

        row.add_child(new St.Icon({
            style_class: 'lumen-row-icon',
            icon_name: result.icon || FALLBACK_ICON,
            icon_size: ROW_ICON_SIZE,
            y_align: Clutter.ActorAlign.CENTER,
        }));

        const text = new St.BoxLayout({
            style_class: 'lumen-row-text',
            vertical: true,
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        text.add_child(new St.Label({
            style_class: 'lumen-row-title',
            text: result.title ?? '',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
        }));
        if (result.subtitle) {
            text.add_child(new St.Label({
                style_class: 'lumen-row-subtitle',
                text: result.subtitle,
                x_expand: true,
                x_align: Clutter.ActorAlign.START,
            }));
        }
        row.add_child(text);

        row.connect('button-press-event', () => {
            this.setSelected(index);
            this._onActivate?.(index);
            return Clutter.EVENT_STOP;
        });

        this._rows[index] = row;
        return row;
    }

    // Rows live in mixed parents (the top-hit group, the section boxes), so measure against the
    // scrolled content rather than the row's own parent.
    _scrollIntoView(row) {
        const adjustment = this._scroll?.get_vscroll_bar()?.get_adjustment();
        if (!adjustment)
            return;

        const [, rowY] = row.get_transformed_position();
        const [, contentY] = this._results.get_transformed_position();
        const top = rowY - contentY;
        const bottom = top + row.get_height();
        const value = adjustment.get_value();
        const page = adjustment.get_page_size();

        if (top < value)
            adjustment.set_value(top);
        else if (bottom > value + page)
            adjustment.set_value(bottom - page);
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

        // The list owns the navigation keys; returns true from `onKey` means "consumed".
        if (this._onKey?.(event.get_key_symbol(), event.get_state(), event))
            return Clutter.EVENT_STOP;

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
