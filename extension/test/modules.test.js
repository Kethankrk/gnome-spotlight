import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readdirSync, readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

import {SCHEMA_ID, SCHEMA_PATH, EXTENSION_UUID} from '../lib/protocol.js';

const extensionDir = join(dirname(fileURLToPath(import.meta.url)), '..');
const libDir = join(extensionDir, 'lib');

const read = (...parts) => readFileSync(join(extensionDir, ...parts), 'utf8');

test('lib modules import no gi://, so node --test can load them', async () => {
    const modules = readdirSync(libDir).filter(name => name.endsWith('.js'));
    assert.ok(modules.length > 0);

    for (const name of modules) {
        assert.doesNotMatch(read('lib', name), /from\s+['"]gi:\/\//, `${name} imports gi://`);
        const module = await import(`../lib/${name}`);
        assert.ok(Object.keys(module).length > 0, `${name} exports nothing`);
    }
});

test('the stylesheet styles every class the overlay asks for', () => {
    const classes = [...read('overlay.js').matchAll(/style_class:\s*'([^']+)'/g)].map(m => m[1]);
    assert.ok(classes.length >= 4, 'the overlay should style a backdrop, a card and its contents');

    const css = read('stylesheet.css');
    for (const name of classes)
        assert.match(css, new RegExp(`\\.${name}[\\s,{:]+`), `.${name} has no rule`);
});

test('the preview pane is built from lib/preview.js, not in the shell glue', () => {
    const overlay = read('overlay.js');
    assert.match(overlay, /from '\.\/lib\/preview\.js'/);
    assert.match(overlay, /buildPreview\(/);
    assert.match(overlay, /PREVIEW_EMPTY_TEXT/);
    // Row index == flat index: the preview follows whatever `setSelected` selected.
    assert.match(overlay, /this\._model\[index\]/);
});

test('the overlay places itself through lib/monitors.js, not a fixed primary monitor', () => {
    const overlay = read('overlay.js');
    assert.match(overlay, /from '\.\/lib\/monitors\.js'/);
    assert.match(overlay, /pickMonitor\(/);
    assert.match(overlay, /centeredOrigin\(/);

    // The shell supplies what a pure module cannot see: the monitor list, the primary and the
    // pointer.
    assert.match(overlay, /Main\.layoutManager\.monitors/);
    assert.match(overlay, /Main\.layoutManager\.primaryMonitor/);
    assert.match(overlay, /global\.get_pointer\(\)/);

    // A monitor added or removed re-runs the geometry while the overlay lives, and destroy()
    // disconnects it (contract §10: plain connect()/disconnect(), no connectObject).
    assert.match(overlay, /'monitors-changed'/);
    assert.match(overlay, /disconnect\(this\._monitorsChangedId\)/);

    // `_syncGeometry` is the single place that positions things: the backdrop and the card.
    const definition = overlay.slice(overlay.indexOf('_syncGeometry() {'));
    const body = definition.slice(0, definition.indexOf('\n    _pointer()'));
    assert.match(body, /pickMonitor\(/);
    assert.match(body, /centeredOrigin\(/);
    assert.equal(body.match(/set_position\(/g)?.length, 2,
        'the backdrop and the card are positioned in `_syncGeometry` and nowhere else');
    assert.equal(overlay.match(/set_position\(/g)?.length, 2);
});

test('the protocol constants match the shipped schema and metadata', () => {
    const schema = read('schemas', `${SCHEMA_ID}.gschema.xml`);
    assert.match(schema, new RegExp(`id="${SCHEMA_ID}"`));
    assert.match(schema, new RegExp(`path="${SCHEMA_PATH}"`));
    assert.match(schema, /name="summon-shortcut"/);

    const metadata = JSON.parse(read('metadata.json'));
    assert.equal(metadata.uuid, EXTENSION_UUID);
    assert.equal(metadata['settings-schema'], SCHEMA_ID);
});

test('the preferences window edits the schema keys through lib/shortcut.js', () => {
    const prefs = read('prefs.js');

    // Adwaita, as GNOME 50 ships it, and the entry point the shell's preferences process calls.
    // The toolkit versions are pinned: with both Gtk 3 and Gtk 4 typelibs installed, an unversioned
    // `gi://Gtk` is ambiguous.
    assert.match(prefs, /from 'gi:\/\/Adw\?version=1'/);
    assert.match(prefs, /from 'gi:\/\/Gtk\?version=4\.0'/);
    assert.match(prefs, /extends ExtensionPreferences/);
    assert.match(prefs, /fillPreferencesWindow\(/);

    // Every key the ticket names is edited, and written straight through: no Apply button.
    const schema = read('schemas', `${SCHEMA_ID}.gschema.xml`);
    for (const key of [
        'summon-shortcut', 'appearance', 'search-engine', 'providers-enabled', 'clipboard-enabled',
    ]) {
        assert.match(prefs, new RegExp(`'${key}'`), `prefs.js does not edit ${key}`);
        assert.match(schema, new RegExp(`name="${key}"`), `the schema has no ${key}`);
    }

    // One switch per provider, from the shared list rather than a hand-written copy.
    assert.match(prefs, /PROVIDER_IDS/);

    // The shortcut decisions come from the Node-tested module, not from ad-hoc string work, and
    // the taken bindings are the shell's and gnome-settings-daemon's.
    assert.match(prefs, /from '\.\/lib\/shortcut\.js'/);
    assert.match(prefs, /flattenBindings\(/);
    assert.match(prefs, /normalizeAccelerator\(/);
    assert.match(prefs, /collides\(/);
    assert.match(prefs, /org\.gnome\.desktop\.wm\.keybindings/);
    assert.match(prefs, /org\.gnome\.settings-daemon\.plugins\.media-keys/);
});

test('the extension applies preferences live, without a shell restart', () => {
    const extension = read('extension.js');

    // The summon shortcut, the appearance, the provider list and clipboard history are all
    // observed, so a change written by the preferences process reaches the running shell.
    for (const constant of ['SUMMON_KEYBINDING', 'APPEARANCE_KEY', 'PROVIDERS_KEY', 'CLIPBOARD_KEY']) {
        assert.match(extension, new RegExp(`changed::\\$\\{${constant}\\}`),
            `${constant} is not observed`);
    }

    // Clipboard history is the daemon's switch (§2): the preference is pushed to it.
    assert.match(extension, /ClipboardSetEnabled/);

    // The appearance choice reaches the overlay.
    assert.match(extension, /setAppearance\(/);
    assert.match(read('overlay.js'), /setAppearance\(/);
});

test('the activation path picks the action by modifier and runs the shell actions it is handed', () => {
    const extension = read('extension.js');

    // Which action a press runs is lib/activation.js's decision, not an ad-hoc `?? 'open'`.
    assert.match(extension, /from '\.\/lib\/activation\.js'/);
    assert.match(extension, /activationAction\(/);
    assert.match(read('lib', 'query.js'), /REVEAL_MODIFIER/);
    assert.match(read('lib', 'query.js'), /COPY_MODIFIER/);

    // The two shell actions of contract §2: `copy` writes the Wayland clipboard, `reveal` shows
    // the file in the desktop's file manager.
    assert.match(extension, /case 'copy':/);
    assert.match(extension, /case 'reveal':/);
    assert.match(extension, /St\.ClipboardType\.CLIPBOARD/);
    assert.match(extension, /org\.freedesktop\.FileManager1/);
    assert.match(extension, /ShowItems/);

    // Clipboard history is forgotten from the list itself (story 48): one entry with Delete, the
    // whole history with Control+Delete, both through the daemon's own methods.
    assert.match(extension, /ClipboardDelete/);
    assert.match(extension, /ClipboardClear/);
    assert.match(read('lib', 'keys.js'), /Delete: 0xffff/);
});

test('result-limit is sent and PageDown asks for the next page of files', () => {
    const extension = read('extension.js');

    // The schema's `result-limit` is what the daemon is asked for, clamped to what it accepts.
    assert.match(extension, /result-limit/);
    assert.match(extension, /clampResultLimit\(/);
    assert.match(extension, /limit: new GLib\.Variant\('u'/);
    // PageDown asks for the next page by offset, so the key is not dead.
    assert.match(extension, /options\.offset = new GLib\.Variant\('u', offset\)/);
    assert.match(extension, /_requestNextPage\(/);
    // The files section has to be able to show the page that was asked for.
    assert.match(read('lib', 'viewmodel.js'), /caps\?\.\[provider\]/);
});

test('the shell glue sets no removed St properties (`spacing` is CSS-only)', () => {
    // GNOME 48 removed the `spacing` GObject property from St.BoxLayout — setting it throws
    // "no property spacing on StBoxLayout" and the whole extension fails to load. It is a CSS
    // property now, so it belongs in the stylesheet and nowhere in the JS.
    assert.doesNotMatch(read('overlay.js'), /\bspacing\s*:/,
        'overlay.js sets `spacing` as a JS property; St reads it from the stylesheet');

    const css = read('stylesheet.css');
    assert.match(css, /\.lumen-card\s*\{[^}]*spacing:/s, '.lumen-card needs its spacing in CSS');
    assert.match(css, /\.lumen-row\s*\{[^}]*spacing:/s, '.lumen-row needs its spacing in CSS');
});

test('the stylesheet themes the overlay for a light and a dark card', () => {
    const css = read('stylesheet.css');
    for (const name of ['lumen-appearance-light', 'lumen-appearance-dark'])
        assert.match(css, new RegExp(`\\.${name}[\\s,{:]+`), `.${name} has no rule`);

    const overlay = read('overlay.js');
    for (const name of ['lumen-appearance-light', 'lumen-appearance-dark'])
        assert.match(overlay, new RegExp(name), `the overlay never asks for ${name}`);
});
