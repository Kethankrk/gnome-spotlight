import {test} from 'node:test';
import assert from 'node:assert/strict';

import {compareByTitle, compareCandidates, compareText} from '../lib/ordering.js';

const row = (title, score, id = `x:${title}`) => ({id, title, score});

test('a higher score comes first, whatever the title', () => {
    const sorted = [row('beta', 0.2), row('alpha', 1.0)].sort(compareCandidates);
    assert.deepEqual(sorted.map(r => r.title), ['alpha', 'beta']);
});

test('equal scores break by title and then by id', () => {
    const sorted = [
        row('beta', 0.5, 'x:b'),
        row('alpha', 0.5, 'x:beta'),
        row('alpha', 0.5, 'x:aaa'),
    ].sort(compareCandidates);

    assert.deepEqual(sorted.map(r => r.id), ['x:aaa', 'x:beta', 'x:b']);
});

test('a missing score or title is not a crash', () => {
    const sorted = [{id: 'x:one'}, row('two', 0.1)].sort(compareCandidates);
    assert.deepEqual(sorted.map(r => r.id), ['x:two', 'x:one']);
    assert.equal(compareCandidates({}, {}), 0);
    assert.equal(compareByTitle(null, null), 0);
});

test('strings are ordered locale-independently', () => {
    assert.equal(compareText('a', 'b'), -1);
    assert.equal(compareText('b', 'a'), 1);
    assert.equal(compareText('a', 'a'), 0);
    assert.equal(compareText(undefined, 'a'), -1, 'a missing value sorts first');
});
