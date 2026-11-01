import {test} from 'node:test';
import assert from 'node:assert/strict';

import {buildRequest, hasSecondWindowModifier, SECOND_WINDOW_MODIFIER} from '../lib/query.js';
import {PROVIDER_IDS} from '../lib/protocol.js';

test('the query text is trimmed', () => {
    assert.equal(buildRequest('  terminal  ', ['applications']).text, 'terminal');
    assert.equal(buildRequest('\tnautilus\n', ['applications']).text, 'nautilus');
});

test('an empty query produces an empty request', () => {
    for (const empty of ['', '   ', '\n\t'])
        assert.deepEqual(buildRequest(empty, PROVIDER_IDS), {text: '', enabled: []});

    assert.deepEqual(buildRequest(null, PROVIDER_IDS), {text: '', enabled: []});
});

test('enabled providers are mapped to contract ids in contract order', () => {
    assert.deepEqual(
        buildRequest('term', ['files', 'applications']).enabled,
        ['applications', 'files'],
    );
});

test('unknown and repeated providers are dropped', () => {
    assert.deepEqual(
        buildRequest('term', ['applications', 'nonsense', 'applications']).enabled,
        ['applications'],
    );
    assert.deepEqual(buildRequest('term', []).enabled, []);
    assert.deepEqual(buildRequest('term').enabled, PROVIDER_IDS);
});

test('the second-window modifier is the control bit of a Clutter mask', () => {
    assert.equal(SECOND_WINDOW_MODIFIER, 1 << 2);
    assert.equal(hasSecondWindowModifier(SECOND_WINDOW_MODIFIER), true);
    assert.equal(hasSecondWindowModifier(1 << 2 | 1 << 6), true, 'Ctrl+Super still counts');
    assert.equal(hasSecondWindowModifier(1 << 0), false, 'shift alone does not');
    assert.equal(hasSecondWindowModifier(0), false);
    assert.equal(hasSecondWindowModifier(null), false);
    assert.equal(hasSecondWindowModifier(undefined), false);
});
