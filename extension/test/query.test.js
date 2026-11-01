import {test} from 'node:test';
import assert from 'node:assert/strict';

import {
    COPY_MODIFIER,
    DEFAULT_RESULT_LIMIT,
    MAX_RESULT_LIMIT,
    REVEAL_MODIFIER,
    buildRequest,
    clampResultLimit,
    hasCopyModifier,
    hasRevealModifier,
    hasSecondWindowModifier,
    SECOND_WINDOW_MODIFIER,
} from '../lib/query.js';
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

test('the reveal and copy modifiers are the bits a file row reads', () => {
    // Control asks for the secondary action (a second window for an application, `Show in Files`
    // for a file); Alt asks for the copy. Both are Clutter masks, spelled as numbers.
    assert.equal(REVEAL_MODIFIER, 1 << 2);
    assert.equal(COPY_MODIFIER, 1 << 3);
    assert.equal(hasRevealModifier(REVEAL_MODIFIER), true);
    assert.equal(hasRevealModifier(COPY_MODIFIER), false);
    assert.equal(hasCopyModifier(COPY_MODIFIER), true);
    assert.equal(hasCopyModifier(COPY_MODIFIER | 1 << 6), true, 'Alt+Super still counts');
    assert.equal(hasCopyModifier(REVEAL_MODIFIER), false);
    assert.equal(hasCopyModifier(0), false);
    assert.equal(hasCopyModifier(null), false);
    assert.equal(hasCopyModifier(undefined), false);
});

test('the page size the daemon is asked for is the schema\'s result-limit, clamped', () => {
    assert.equal(clampResultLimit(30), 30);
    assert.equal(clampResultLimit(1), 1);
    assert.equal(clampResultLimit(0), 1, 'a page of nothing is not a page');
    assert.equal(clampResultLimit(-5), 1);
    assert.equal(clampResultLimit(1000), MAX_RESULT_LIMIT, 'the daemon clamps here too');
    assert.equal(clampResultLimit(7.9), 7, 'a whole number of results');

    for (const junk of [undefined, null, NaN, 'nonsense', {}, Infinity])
        assert.equal(clampResultLimit(junk), DEFAULT_RESULT_LIMIT, `${junk} falls back`);
});
