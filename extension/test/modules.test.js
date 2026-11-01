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

test('the protocol constants match the shipped schema and metadata', () => {
    const schema = read('schemas', `${SCHEMA_ID}.gschema.xml`);
    assert.match(schema, new RegExp(`id="${SCHEMA_ID}"`));
    assert.match(schema, new RegExp(`path="${SCHEMA_PATH}"`));
    assert.match(schema, /name="summon-shortcut"/);

    const metadata = JSON.parse(read('metadata.json'));
    assert.equal(metadata.uuid, EXTENSION_UUID);
    assert.equal(metadata['settings-schema'], SCHEMA_ID);
});
