import {test} from 'node:test';
import assert from 'node:assert/strict';

import {
    BUS_NAME, OBJECT_PATH, INTERFACE, PROTOCOL_MAJOR, SUPPORTED_MAJOR,
    PROVIDER_IDS, CAPABILITY_IDS, SECTION_ORDER, SECTION_CAPS, ID_PREFIXES,
    providerOfId, PROVIDER_LABELS, ACTION_HANDLERS, SHELL_ACTIONS,
} from '../lib/protocol.js';

test('the identity constants are the contract identity', () => {
    assert.equal(BUS_NAME, 'org.gnome.Lumen');
    assert.equal(OBJECT_PATH, '/org/gnome/Lumen');
    assert.equal(INTERFACE, 'org.gnome.Lumen.Engine');
    assert.equal(PROTOCOL_MAJOR, 1);
    assert.equal(SUPPORTED_MAJOR, PROTOCOL_MAJOR);
});

test('every provider has a label, a cap, an id prefix and a section slot', () => {
    assert.equal(PROVIDER_IDS.length, 8);
    for (const provider of PROVIDER_IDS) {
        assert.ok(PROVIDER_LABELS[provider], `${provider} needs a label`);
        assert.equal(typeof SECTION_CAPS[provider], 'number');
        assert.ok(SECTION_ORDER.includes(provider), `${provider} needs a section`);
        assert.ok(ID_PREFIXES[provider], `${provider} needs an id prefix`);
    }
});

test('every capability id is a provider id', () => {
    for (const id of CAPABILITY_IDS)
        assert.ok(PROVIDER_IDS.includes(id), `${id} is not a provider`);
});

test('providerOfId maps a result id back to its provider', () => {
    assert.equal(providerOfId('app:org.gnome.Nautilus.desktop'), 'applications');
    assert.equal(providerOfId('file:/home/me/notes.md'), 'files');
    assert.equal(providerOfId('web:abc123'), 'websearch');
    assert.equal(providerOfId('nonsense'), null);
});

test('the action handlers are the contract set', () => {
    assert.deepEqual(ACTION_HANDLERS, ['open', 'copy', 'reveal', 'shell']);
});

test('the shell actions are the ones the daemon hands back to the extension', () => {
    // `open-url` opens the browser, `copy` writes the clipboard and `reveal` shows the file in the
    // file manager. The last two name a row handler as well; `open-url` is only ever a routed
    // action, never something a row carries.
    assert.deepEqual(SHELL_ACTIONS, ['open-url', 'copy', 'reveal']);
    assert.ok(!ACTION_HANDLERS.includes('open-url'));
});
