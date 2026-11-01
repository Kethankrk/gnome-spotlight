import {test} from 'node:test';
import assert from 'node:assert/strict';

import {resultFromRow, resultsFromRows, unpack} from '../lib/wire.js';

test('a full result row keeps every contract field', () => {
    assert.deepEqual(resultFromRow({
        id: 'app:org.gnome.Nautilus.desktop',
        provider: 'applications',
        title: 'Files',
        subtitle: 'Browse your files',
        icon: 'org.gnome.Nautilus',
        score: 1.3,
        actions: [{id: 'open', label: 'Open', handler: 'open'}],
        preview: {kind: 'application', comment: 'Browse your files'},
    }), {
        id: 'app:org.gnome.Nautilus.desktop',
        provider: 'applications',
        title: 'Files',
        subtitle: 'Browse your files',
        icon: 'org.gnome.Nautilus',
        score: 1.3,
        actions: [{id: 'open', label: 'Open', handler: 'open'}],
        preview: {kind: 'application', comment: 'Browse your files'},
    });
});

test('a sparse row defaults instead of yielding undefined', () => {
    assert.deepEqual(resultFromRow({id: 'file:/x', provider: 'files'}), {
        id: 'file:/x',
        provider: 'files',
        title: '',
        subtitle: '',
        icon: '',
        score: 0,
        actions: [],
        preview: {},
    });
    assert.equal(resultFromRow(null).provider, '');
});

test('the score is coerced to a number', () => {
    assert.equal(resultFromRow({score: '0.75'}).score, 0.75);
    assert.equal(resultFromRow({score: null}).score, 0);
});

test('unpacking is idempotent for plain values and walks nested objects and arrays', () => {
    const plain = {id: 'app:x', actions: [{id: 'open'}], preview: {kind: 'application'}};
    assert.deepEqual(unpack(plain), plain);
    assert.equal(unpack('x'), 'x');
    assert.equal(unpack(7), 7);
    assert.equal(unpack(null), null);
});

test('rows map through resultsFromRows', () => {
    assert.deepEqual(resultsFromRows([{id: 'a'}, {id: 'b'}]).map(result => result.id), ['a', 'b']);
    assert.deepEqual(resultsFromRows(null), []);
});
