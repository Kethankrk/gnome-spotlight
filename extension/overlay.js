// The summon overlay: a full-screen transparent backdrop with a translucent, rounded card in
// front of it. Everything here is shell-only (St/Clutter/Main); the testable model lives in
// lib/. Ticket 03 grows a results list and preview inside `card`; ticket 04 fills the preview
// pane below the list (its contents come from lib/preview.js); ticket 15 places the backdrop and
// the card on the monitor under the pointer (lib/monitors.js decides which one). Ticket 14 names
// every part for a screen reader, gives the parts an Atk role, and gates the card's entrance
// animation on the user's motion preference (lib/a11y.js decides both).

import Atk from 'gi://Atk';
import Clutter from 'gi://Clutter';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {
    CARD_ACCESSIBLE_NAME,
    ENTRY_ACCESSIBLE_NAME,
    HIGH_CONTRAST_CLASS,
    PREVIEW_ACCESSIBLE_NAME,
    RESULTS_ACCESSIBLE_NAME,
    accessibleNameFor,
    sectionAccessibleName,
    shouldAnimate,
} from './lib/a11y.js';
import {ENGINE_NOT_RUNNING} from './lib/engine.js';
import {centeredOrigin, pickMonitor} from './lib/monitors.js';
import {buildPreview, PREVIEW_EMPTY_TEXT} from './lib/preview.js';

const CARD_MAX_WIDTH = 640;
const MONITOR_MARGIN = 80;
const ROW_ICON_SIZE = 24;
const FALLBACK_ICON = 'application-x-executable';
// Short enough to read as a settle, long enough that the card is visibly moving when motion is
// allowed — and never run at all when the user asked for less of it.
const FADE_DURATION_MS = 140;

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
        this._cardSizeId = 0;
        this._buttonPressId = 0;
        this._syncing = false;
        this._rows = [];
        this._selectedRow = null;
        this._model = [];
        this._appearance = 'system';
        this._systemSettings = null;
        this._systemSettingsId = 0;
        // Motion is allowed until the shell says otherwise, which is GNOME's own default.
        this._shouldAnimate = true;
        this._highContrast = false;

        this._card = new St.BoxLayout({
            style_class: 'lumen-card',
            vertical: true,
            reactive: true,
            spacing: 4,
        });
        // A modal search surface for a screen reader: the card is the thing that appeared, and
        // everything below it hangs off it.
        this._card.set_accessible_role(Atk.Role.DIALOG);
        this._card.set_accessible_name(CARD_ACCESSIBLE_NAME);

        this._entry = new St.Entry({
            style_class: 'lumen-entry',
            x_expand: true,
            can_focus: true,
            hint_text: 'Search…',
        });
        this._entry.set_accessible_role(Atk.Role.ENTRY);
        this._entry.set_accessible_name(ENTRY_ACCESSIBLE_NAME);

        this._status = new St.Label({
            style_class: 'lumen-status',
            x_expand: true,
            text: ENGINE_NOT_RUNNING,
        });
        this._status.set_accessible_role(Atk.Role.LABEL);
        this._status.set_accessible_name(ENGINE_NOT_RUNNING);

        this._results = new St.BoxLayout({
            style_class: 'lumen-results',
            vertical: true,
            x_expand: true,
        });
        this._results.set_accessible_role(Atk.Role.LIST);
        this._results.set_accessible_name(RESULTS_ACCESSIBLE_NAME);

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

        // The preview pane sits below the result list and mirrors the selected row. It is hidden
        // while nothing is selected and shows a muted line when the row has nothing to show.
        this._preview = new St.BoxLayout({
            style_class: 'lumen-preview',
            vertical: true,
            x_expand: true,
            visible: false,
        });
        // The pane is a region of its own, so a screen reader announces it as one instead of
        // reading its rows as more results.
        this._preview.set_accessible_role(Atk.Role.PANEL);
        this._preview.set_accessible_name(PREVIEW_ACCESSIBLE_NAME);

        this._previewTitle = new St.Label({
            style_class: 'lumen-preview-title',
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
        });

        this._previewRows = new St.BoxLayout({
            style_class: 'lumen-preview-rows',
            vertical: true,
            x_expand: true,
        });

        this._previewEmpty = new St.Label({
            style_class: 'lumen-preview-empty',
            text: PREVIEW_EMPTY_TEXT,
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
            visible: false,
        });

        this._preview.add_child(this._previewTitle);
        this._preview.add_child(this._previewRows);
        this._preview.add_child(this._previewEmpty);
        this._card.add_child(this._preview);


        // A fixed layout, not a BinLayout: `_syncGeometry` is the single place that positions the
        // card, so the layout must not re-centre it behind our back.
        this._backdrop = new St.Widget({
            style_class: 'lumen-backdrop',
            reactive: true,
            visible: false,
            layout_manager: new Clutter.FixedLayout(),
        });
        this._backdrop.add_child(this._card);

        // Adding or removing a monitor moves the overlay: recompute while the overlay lives (it is
        // constructed in `enable` and destroyed in `disable`) and disconnect in `destroy`. The
        // card's own height changes as rows come and go, and it is centred, so it moves with them.
        this._buttonPressId = this._backdrop.connect(
            'button-press-event', (...args) => this._onButtonPress(...args));
        this._cardSizeId = this._card.connect('notify::height', () => this._syncGeometry());
        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed', () => this._syncGeometry());

        // `appearance: system` follows the desktop's colour scheme. The shell mirrors that setting
        // in St.Settings, so no second GSettings object is needed — and a build that cannot answer
        // (guarded below) keeps the built-in card.
        try {
            this._systemSettings = St.Settings.get();
            if (this._systemSettings)
                this._systemSettingsId = this._systemSettings.connect(
                    'notify::color-scheme', () => this._applyAppearance());
        } catch (error) {
            logError(error, 'Lumen: no St.Settings, the overlay appearance follows the theme');
        }

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
        this._fadeIn();

        this._grab = Main.pushModal(this._backdrop) ?? null;
        this._entry.grab_key_focus();

        this._capturedEventId = global.stage.connect(
            'captured-event', (...args) => this._onCapturedEvent(...args));
        this._keyFocusId = global.stage.connect(
            'notify::key-focus', () => this._onKeyFocusChanged());
    }

    // The card's entrance. With motion allowed the card settles in; without it the card is simply
    // there, which is the whole of reduced motion here (lib/a11y.js decides; contract §18). Only
    // the entrance animates — closing is immediate, so a grab released in `close()` is never held
    // by an animation that is still running.
    _fadeIn() {
        const card = this._card;
        if (!card)
            return;

        if (!this._shouldAnimate) {
            card.set_opacity(255);
            return;
        }

        try {
            card.remove_transition?.('opacity');
            card.set_opacity(0);
            card.ease({
                opacity: 255,
                duration: FADE_DURATION_MS,
                mode: Clutter.AnimationMode.EASE_OUT_QUAD,
            });
        } catch (error) {
            logError(error, 'Lumen: could not animate the overlay in');
            card.set_opacity(255);
        }
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
        const message = text ?? '';
        this._status.set_text(message);
        // The status line is where a failure or the engine state is reported, so a screen reader
        // needs to read it as well as see it.
        this._status.set_accessible_name(message);
    }

    /**
     * The user's accessibility preferences, from lib/a11y.js's `accessibilityState`: whether motion
     * is allowed and whether the desktop is in high contrast. Either can change while the overlay is
     * open, so this reaches the live actors rather than only the next open.
     */
    setAccessibility(state = {}) {
        this._shouldAnimate = shouldAnimate(state);
        this._highContrast = state?.highContrast === true;
        this._applyHighContrast();
    }

    /**
     * The overlay's appearance, from the `appearance` preference (contract §8): `light`, `dark`, or
     * anything else for "follow the system". Only a style class on the backdrop changes, so it
     * applies to an open overlay as well as to the next one.
     */
    setAppearance(mode) {
        this._appearance = mode === 'light' || mode === 'dark' ? mode : 'system';
        this._applyAppearance();
    }

    /**
     * Render a view model from lib/viewmodel.js. Row indices are the `flat` model order: the Top
     * Hit first, then each section. Clicking a row calls `onActivate(index)`.
     */
    setResults(model = {}) {
        const {topHit = null, sections = []} = model;
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
            this._results.add_child(this._createSectionHeader(section));
            for (const item of section.items ?? [])
                this._results.add_child(this._createRow(item, index++));
        }

        // Row index == flat index, so `flat` is also what the preview pane looks the selected row
        // up in. Falling back to the same ordering covers a caller that only passes topHit/sections.
        this._model = Array.isArray(model.flat)
            ? model.flat
            : [topHit, ...sections.flatMap(section => section.items ?? [])].filter(item => item);

        this.setSelected(this._rows.length > 0 ? 0 : -1);
    }

    /** Highlight row `index` (a `flat` index), scroll it into view and preview it. */
    setSelected(index) {
        this._selectedRow?.remove_style_class_name('lumen-row-selected');
        this._selectedRow = null;

        const row = this._rows[index] ?? null;
        if (!row) {
            this._setPreview(null);
            return;
        }

        row.add_style_class_name('lumen-row-selected');
        this._selectedRow = row;
        this._scrollIntoView(row);
        this._setPreview(this._model[index] ?? null);
    }

    destroy() {
        this.close();

        if (this._monitorsChangedId) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }

        if (this._cardSizeId) {
            this._card?.disconnect(this._cardSizeId);
            this._cardSizeId = 0;
        }

        if (this._buttonPressId) {
            this._backdrop.disconnect(this._buttonPressId);
            this._buttonPressId = 0;
        }

        if (this._systemSettingsId) {
            this._systemSettings?.disconnect(this._systemSettingsId);
            this._systemSettingsId = 0;
        }
        this._systemSettings = null;

        this._backdrop.get_parent()?.remove_child(this._backdrop);
        this._backdrop.destroy();
        this._backdrop = null;
        this._card = null;
        this._entry = null;
        this._status = null;
        this._scroll = null;
        this._results = null;
        this._preview = null;
        this._previewTitle = null;
        this._previewRows = null;
        this._previewEmpty = null;
        this._rows = [];
        this._selectedRow = null;
        this._model = [];
        this._onClose = null;
        this._onActivate = null;
        this._onKey = null;
    }

    _clearRows() {
        for (const child of this._results.get_children())
            child.destroy();

        this._rows = [];
        this._selectedRow = null;
        this._model = [];
    }

    // The pane follows the selection: `null` hides it, a result with no detail keeps it visible
    // with the muted empty state (contract §6, story 53).
    _setPreview(result) {
        if (!this._preview)
            return;

        for (const child of this._previewRows.get_children())
            child.destroy();

        if (!result) {
            this._preview.hide();
            return;
        }

        const {title, rows} = buildPreview(result);
        this._previewTitle.set_text(title);
        this._previewTitle.visible = title !== '';
        this._previewEmpty.visible = rows.length === 0;

        for (const {label, value} of rows)
            this._previewRows.add_child(this._createPreviewRow(label, value));

        this._preview.show();
    }

    _createPreviewRow(label, value) {
        const row = new St.BoxLayout({
            style_class: 'lumen-preview-row',
            vertical: false,
            x_expand: true,
        });

        row.add_child(new St.Label({
            style_class: 'lumen-preview-label',
            text: label,
            y_align: Clutter.ActorAlign.START,
        }));
        row.add_child(new St.Label({
            style_class: 'lumen-preview-value',
            text: value,
            line_wrap: true,
            x_expand: true,
            x_align: Clutter.ActorAlign.START,
        }));

        return row;
    }

    // A labelled group of rows. The header is read out as a heading, named from the section's own
    // label (lib/a11y.js), so the sections are distinguishable when the list is walked by a reader.
    _createSectionHeader(section) {
        const label = section?.label ?? section?.provider ?? '';
        const header = new St.Label({
            style_class: 'lumen-section-header',
            text: label,
            x_align: Clutter.ActorAlign.START,
        });

        header.set_accessible_role(Atk.Role.HEADING);
        header.set_accessible_name(sectionAccessibleName(section));
        return header;
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

        // The row is what a reader lands on, so it carries the list-item role and a name built
        // from the title and the subtitle it shows.
        row.set_accessible_role(Atk.Role.LIST_ITEM);
        row.set_accessible_name(accessibleNameFor(result));

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

    // The one place that positions the card. The backdrop covers the monitor the overlay belongs
    // on — the one under the pointer, else the primary, else the first (lib/monitors.js) — and the
    // card is centred inside it. `centeredOrigin` gives screen coordinates, so the card (a child of
    // the monitor-sized backdrop) is positioned relative to the monitor's top-left. Re-entrant
    // calls are dropped: allocating the card can notify its height, which calls straight back here.
    _syncGeometry() {
        if (this._syncing)
            return;

        const monitors = Main.layoutManager.monitors ?? [];
        const index = pickMonitor(monitors, this._pointer(), {
            primary: Main.layoutManager.primaryMonitor ?? null,
        });
        const monitor = monitors[index] ?? null;
        if (!monitor)
            return;

        this._syncing = true;
        try {
            this._backdrop.set_position(monitor.x, monitor.y);
            this._backdrop.set_size(monitor.width, monitor.height);

            const width = Math.min(CARD_MAX_WIDTH, Math.max(320, monitor.width - MONITOR_MARGIN));
            this._card.set_width(width);

            const [, naturalHeight] = this._card.get_preferred_height(width);
            const height = Math.max(0, naturalHeight || this._card.height || 0);
            const origin = centeredOrigin(monitor, {width, height});

            // The card cannot be taller than the monitor, so a card that would overflow anchors to
            // the monitor's top instead of being clipped at both ends.
            this._card.set_position(
                Math.max(0, origin.x - monitor.x),
                Math.max(0, Math.min(origin.y - monitor.y, monitor.height - height)));
        } finally {
            this._syncing = false;
        }
    }

    /** The pointer in stage coordinates; lib/monitors.js ignores it when it is not a real point. */
    _pointer() {
        const [x, y] = global.get_pointer();
        return {x, y};
    }

    // Only the card's look changes with the appearance, and it changes by style class: the class
    // goes on the backdrop so the stylesheet can reach the card and everything in it.
    _applyAppearance() {
        if (!this._backdrop)
            return;

        const dark = this._appearance === 'dark' ? true
            : this._appearance === 'light' ? false
                : this._systemDark();

        this._backdrop.remove_style_class_name('lumen-appearance-light');
        this._backdrop.remove_style_class_name('lumen-appearance-dark');

        if (dark === true)
            this._backdrop.add_style_class_name('lumen-appearance-dark');
        else if (dark === false)
            this._backdrop.add_style_class_name('lumen-appearance-light');
    }

    // High contrast is a style class on the backdrop, next to the appearance classes, so the
    // stylesheet can raise the contrast of the card and everything inside it (ticket 14).
    _applyHighContrast() {
        if (!this._backdrop)
            return;

        if (this._highContrast)
            this._backdrop.add_style_class_name(HIGH_CONTRAST_CLASS);
        else
            this._backdrop.remove_style_class_name(HIGH_CONTRAST_CLASS);
    }

    // What the desktop asked for, or `null` when it cannot be asked. A desktop that has never been
    // asked for a light scheme keeps the built-in card, which is what the overlay looked like
    // before there was a choice; only an explicit `prefer-light` turns the card light.
    _systemDark() {
        try {
            const scheme = this._systemSettings?.color_scheme;
            if (scheme === undefined || !St.SystemColorScheme)
                return null;

            return scheme !== St.SystemColorScheme.PREFER_LIGHT;
        } catch (error) {
            return null;
        }
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
