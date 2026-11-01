// The Lumen preferences window (contract §8, ticket 12).
//
// Adwaita, and therefore shell-only: this file runs in the *preferences* process (`gnome-extensions
// prefs`), never inside gnome-shell, and is never imported by the Node tests. Everything it does
// that can be got wrong — reading, normalising, labelling and collision-checking a GTK accelerator
// — lives in `lib/shortcut.js`, which is Node-tested (`extension/test/shortcut.test.js`). What is
// left here is widget glue: capture a key combination, write it, say what is wrong with it.
//
// Four schema keys are edited, and each is written to GSettings the moment it changes, so the
// running extension sees it through `changed::` and no shell restart is needed:
//
//   `summon-shortcut`      a shortcut editor, with an inline warning when the combination is
//                          already grabbed by the shell or by gnome-settings-daemon
//   `appearance`           system / light / dark, applied to the overlay live
//   `providers-enabled`    one switch per provider in `PROVIDER_IDS`
//   `clipboard-enabled`    turns history recording on and off, live
//
// The existing bindings the collision warning is checked against are gathered **once, when the
// window opens** (the accelerators an extension may shadow do not move while the user is looking
// at the page), from `org.gnome.desktop.wm.keybindings`, the media-keys schema
// (`org.gnome.settings-daemon.plugins.media-keys`, plus the custom keybindings it points at) and
// `org.gnome.shell.keybindings`. `lib/shortcut.js` flattens those groups into one list of taken
// accelerators.

// The versions are pinned: both Gtk 3 and Gtk 4 typelibs are installed on a normal GNOME machine,
// and an unversioned `gi://Gtk` is ambiguous (GJS warns and picks one), which in a process that
// also loads libadwaita means mixing two toolkits.
import Adw from 'gi://Adw?version=1';
import Gdk from 'gi://Gdk?version=4.0';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Gtk from 'gi://Gtk?version=4.0';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {APPEARANCE_MODES, PROVIDER_IDS, PROVIDER_LABELS} from './lib/protocol.js';
import {
    DISABLED_ACCELERATOR,
    acceleratorLabel,
    bindingLabel,
    collides,
    flattenBindings,
    normalizeAccelerator,
    prettifyKeyName,
} from './lib/shortcut.js';

const SUMMON_KEY = 'summon-shortcut';
const APPEARANCE_KEY = 'appearance';
const PROVIDERS_KEY = 'providers-enabled';
const CLIPBOARD_KEY = 'clipboard-enabled';

const WM_KEYBINDINGS_SCHEMA = 'org.gnome.desktop.wm.keybindings';
const MEDIA_KEYS_SCHEMA = 'org.gnome.settings-daemon.plugins.media-keys';
const SHELL_KEYBINDINGS_SCHEMA = 'org.gnome.shell.keybindings';
const CUSTOM_KEYBINDING_SCHEMA = 'org.gnome.settings-daemon.plugins.media-keys.custom-keybinding';
const CUSTOM_KEYBINDINGS_KEY = 'custom-keybindings';

const APPEARANCE_LABELS = {
    system: 'System',
    light: 'Light',
    dark: 'Dark',
};

const PROVIDER_HINTS = {
    applications: 'Installed applications, ranked by how often you pick them',
    files: 'Files and folders indexed by LocalSearch',
    recent: 'Documents you opened recently',
    clipboard: 'Text you copied (needs clipboard history)',
    calculator: 'Arithmetic and unit conversions',
    actions: 'Lock, suspend and log out',
    settings: 'Wi-Fi, Bluetooth, Do Not Disturb and dark mode',
    websearch: 'A web-search row when nothing else matches',
};

const SHORTCUT_HINT = 'Click to record a new key combination';
const RECORDING_HINT = 'Press the combination you want, or Escape to cancel';

// Keys that are only modifiers. Pressing one is the user holding it down on the way to the real
// combination, so it must not end the recording. `Gdk.keyval_is_modifier()` is not introspectable
// in Gdk 4, so the names are matched instead — `Gdk.keyval_name()` is available.
const MODIFIER_KEY_NAMES = new Set([
    'Shift_L', 'Shift_R', 'Control_L', 'Control_R', 'Alt_L', 'Alt_R', 'Meta_L', 'Meta_R',
    'Super_L', 'Super_R', 'Hyper_L', 'Hyper_R', 'Caps_Lock', 'Shift_Lock', 'Mode_switch',
    'ISO_Level3_Shift', 'ISO_Level5_Shift',
]);

export default class LumenPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        this._settings = this.getSettings();
        this._window = window;
        this._capturing = false;
        this._captureController = null;
        this._taken = takenBindings();

        window.search_enabled = true;
        window.add(this._generalPage());
        window.add(this._providersPage());

        this._settingsChangedId = this._settings.connect(
            `changed::${SUMMON_KEY}`, () => this._showShortcut());
        this._showShortcut();
    }

    _generalPage() {
        const page = new Adw.PreferencesPage({
            title: 'General',
            icon_name: 'preferences-system-symbolic',
        });

        const summon = new Adw.PreferencesGroup({
            title: 'Summon Lumen',
            description: 'The key combination that opens Lumen. It works in every window, '
                + 'including full-screen ones.',
        });

        this._shortcutLabel = new Gtk.ShortcutLabel({
            disabled_text: DISABLED_ACCELERATOR,
            valign: Gtk.Align.CENTER,
        });

        const record = new Gtk.Button({
            child: this._shortcutLabel,
            valign: Gtk.Align.CENTER,
            tooltip_text: SHORTCUT_HINT,
        });
        record.connect('clicked', () => this._startCapture());

        this._shortcutRow = new Adw.ActionRow({
            title: 'Keyboard shortcut',
            subtitle: SHORTCUT_HINT,
            activatable_widget: record,
        });
        this._shortcutRow.add_suffix(record);
        summon.add(this._shortcutRow);

        // The collision warning sits under the row it is about. It is a plain label with
        // libadwaita's `warning` style class, inside a box that supplies the group's padding.
        this._warning = new Gtk.Label({wrap: true, xalign: 0});
        this._warning.add_css_class('warning');
        this._warning.add_css_class('caption');

        this._warningBox = new Gtk.Box({
            orientation: Gtk.Orientation.VERTICAL,
            margin_start: 12,
            margin_end: 12,
            margin_bottom: 6,
            visible: false,
        });
        this._warningBox.append(this._warning);
        summon.add(this._warningBox);

        page.add(summon);
        page.add(this._appearanceGroup());
        page.add(this._clipboardGroup());
        return page;
    }

    _appearanceGroup() {
        const group = new Adw.PreferencesGroup({
            title: 'Appearance',
            description: 'How the Lumen overlay is drawn. “System” follows the desktop’s dark-mode '
                + 'setting.',
        });

        const combo = new Adw.ComboRow({
            title: 'Appearance',
            model: Gtk.StringList.new(APPEARANCE_MODES.map(mode => APPEARANCE_LABELS[mode])),
        });

        const current = this._settings.get_string(APPEARANCE_KEY);
        const selected = APPEARANCE_MODES.indexOf(current);
        combo.selected = selected >= 0 ? selected : 0;

        combo.connect('notify::selected', () => {
            const mode = APPEARANCE_MODES[combo.selected];
            if (mode)
                this._settings.set_string(APPEARANCE_KEY, mode);
        });

        group.add(combo);
        return group;
    }

    _clipboardGroup() {
        const group = new Adw.PreferencesGroup({title: 'Clipboard'});

        const row = new Adw.SwitchRow({
            title: 'Clipboard history',
            subtitle: 'Keep a local, encrypted history of the text you copy, newest first. '
                + 'Off by default.',
        });
        this._settings.bind(CLIPBOARD_KEY, row, 'active', Gio.SettingsBindFlags.DEFAULT);

        group.add(row);
        return group;
    }

    _providersPage() {
        const page = new Adw.PreferencesPage({
            title: 'Providers',
            icon_name: 'view-list-symbolic',
        });

        const group = new Adw.PreferencesGroup({
            title: 'Result providers',
            description: 'Turn off the kinds of results you never want. A provider that is off is '
                + 'not searched, and its section is not shown.',
        });

        const enabled = new Set(this._settings.get_strv(PROVIDERS_KEY));
        for (const provider of PROVIDER_IDS) {
            const row = new Adw.SwitchRow({
                title: PROVIDER_LABELS[provider] ?? provider,
                subtitle: PROVIDER_HINTS[provider] ?? '',
            });
            row.active = enabled.has(provider);
            row.connect('notify::active', () => this._setProviderEnabled(provider, row.active));
            group.add(row);
        }

        page.add(group);
        return page;
    }

    // The stored list stays deduped and in contract order (§7), so a switch cannot reorder the
    // sections or leave a provider in twice.
    _setProviderEnabled(provider, enabled) {
        const providers = new Set(this._settings.get_strv(PROVIDERS_KEY));
        if (enabled)
            providers.add(provider);
        else
            providers.delete(provider);

        this._settings.set_strv(
            PROVIDERS_KEY, PROVIDER_IDS.filter(id => providers.has(id)));
    }

    _showShortcut() {
        if (this._capturing)
            return;

        const value = this._settings.get_string(SUMMON_KEY);
        this._shortcutLabel.accelerator = value;

        const clash = collides(value, {existing: this._taken});
        if (!clash) {
            this._warn(null);
            return;
        }

        const name = acceleratorLabel(value) ?? value;
        this._warn(`${name} is already used by ${bindingLabel(clash) ?? 'another shortcut'} — `
            + 'Lumen may not receive it.');
    }

    _warn(message) {
        this._warning.label = message ?? '';
        this._warningBox.visible = Boolean(message);
    }

    // Recording starts on the next idle turn: a button activated from the keyboard would otherwise
    // capture the very key that activated it (Space, Return) as the new shortcut.
    _startCapture() {
        if (this._capturing)
            return;

        GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            this._beginCapture();
            return GLib.SOURCE_REMOVE;
        });
    }

    _beginCapture() {
        if (this._capturing || !this._window)
            return;

        this._capturing = true;
        this._shortcutLabel.accelerator = '';
        this._shortcutLabel.disabled_text = 'Press a combination…';
        this._shortcutRow.subtitle = RECORDING_HINT;
        this._warn(null);

        this._captureController = new Gtk.EventControllerKey();
        // Capture phase: Escape has to cancel the recording, not close the window.
        this._captureController.set_propagation_phase(Gtk.PropagationPhase.CAPTURE);
        this._captureController.connect('key-pressed', (_controller, keyval, _keycode, state) =>
            this._onCapturedKey(keyval, state));
        this._window.add_controller(this._captureController);
    }

    _endCapture() {
        if (!this._capturing)
            return;

        this._capturing = false;

        if (this._captureController) {
            this._window?.remove_controller(this._captureController);
            this._captureController = null;
        }

        this._shortcutRow.subtitle = SHORTCUT_HINT;
        this._shortcutLabel.disabled_text = DISABLED_ACCELERATOR;
        // Put the stored combination back; `_setShortcut` writes through here as well, so a
        // combination that is written ends up on the button exactly once.
        this._showShortcut();
    }

    // Returns true (GDK_EVENT_STOP) for every key while recording, so nothing reaches the window.
    _onCapturedKey(keyval, state) {
        if (!this._capturing)
            return false;

        if (keyval === Gdk.KEY_Escape) {
            this._endCapture();
            return true;
        }

        // A modifier on its own — the user is holding it down on the way to the real key.
        if (MODIFIER_KEY_NAMES.has(Gdk.keyval_name(keyval) ?? ''))
            return true;

        const modifiers = state & Gtk.accelerator_get_default_mod_mask();
        if (!Gtk.accelerator_valid(keyval, modifiers)) {
            this._warn(`${Gdk.keyval_name(keyval) ?? 'That key'} cannot be used as a shortcut. `
                + 'Press another combination.');
            return true;
        }

        const accelerator = Gtk.accelerator_name(keyval, modifiers);
        this._endCapture();

        if (accelerator)
            this._setShortcut(accelerator);

        return true;
    }

    _setShortcut(accelerator) {
        const normalized = normalizeAccelerator(accelerator);
        if (!normalized) {
            this._warn('That combination is not a shortcut Lumen can use.');
            return;
        }

        // Writing is what the running extension listens for (`changed::summon-shortcut`), and the
        // signal handler is also what re-reads the button label and the collision warning.
        this._settings.set_string(SUMMON_KEY, normalized);
    }
}

// Every binding the shell and gnome-settings-daemon already grab, as the `{label, accelerators}`
// groups `flattenBindings` wants.
function takenBindings() {
    const groups = [
        ...bindingGroups(WM_KEYBINDINGS_SCHEMA),
        ...bindingGroups(MEDIA_KEYS_SCHEMA, [CUSTOM_KEYBINDINGS_KEY]),
        ...bindingGroups(SHELL_KEYBINDINGS_SCHEMA),
        ...customBindingGroups(),
    ];

    return flattenBindings(groups);
}

// The string-array keys of one non-relocatable schema. A key that is not a list of accelerators
// (the media-keys schema also holds booleans, ints and commands) is skipped, as is a schema this
// machine does not have.
function bindingGroups(schemaId, skip = []) {
    const settings = settingsFor(schemaId);
    const schema = settings?.settings_schema ?? null;
    if (!schema)
        return [];

    const groups = [];
    for (const key of schema.list_keys()) {
        if (skip.includes(key))
            continue;

        try {
            const value = settings.get_value(key);
            if (value.get_type_string() !== 'as')
                continue;

            const accelerators = value.deep_unpack().filter(entry => typeof entry === 'string');
            if (accelerators.length > 0)
                groups.push({label: prettifyKeyName(key), accelerators});
        } catch (error) {
            logError(error, `Lumen: could not read ${schemaId} ${key}`);
        }
    }

    return groups;
}

// Custom shortcuts are relocatable settings under `custom-keybindings`, and their `binding` key is
// one accelerator, not a list — worth following, because a custom shortcut is exactly the kind of
// binding a user has already taken.
function customBindingGroups() {
    const settings = settingsFor(MEDIA_KEYS_SCHEMA);
    if (!settings)
        return [];

    const groups = [];
    let paths = [];
    try {
        paths = settings.get_strv(CUSTOM_KEYBINDINGS_KEY);
    } catch (error) {
        logError(error, 'Lumen: could not read the custom keybindings');
        return [];
    }

    for (const path of paths) {
        const custom = settingsFor(CUSTOM_KEYBINDING_SCHEMA, path);
        if (!custom)
            continue;

        try {
            groups.push({
                label: custom.get_string('name') || prettifyKeyName('custom keybinding'),
                accelerators: [custom.get_string('binding')],
            });
        } catch (error) {
            logError(error, `Lumen: could not read the custom keybinding at ${path}`);
        }
    }

    return groups;
}

function settingsFor(schemaId, path = null) {
    try {
        const schema = Gio.SettingsSchemaSource.get_default()?.lookup(schemaId, true) ?? null;
        if (!schema)
            return null;

        // `settings_schema` is the property `new Gio.Settings({schema_id})` uses; looking the
        // schema up first is what lets a relocatable one (the custom keybindings) be opened too.
        const properties = {settings_schema: schema};
        if (path)
            properties.path = path;

        return new Gio.Settings(properties);
    } catch (error) {
        logError(error, `Lumen: no settings for ${schemaId}`);
        return null;
    }
}
