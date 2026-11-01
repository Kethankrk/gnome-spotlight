// Clipboard capture decisions, without a shell. See `extension/lib/clipboard.js` for why the shell
// samples the clipboard instead of listening for `St.Clipboard`'s `owner-changed` signal.

import test from 'node:test';
import assert from 'node:assert/strict';

import {
    createClipboardSampler,
    pushPayload,
    shouldCapture,
    CLIPBOARD_PROVIDER,
    DISABLED_RESULT_ID,
} from '../lib/clipboard.js';

test('nothing is captured while history is off or the engine is away', () => {
    assert.equal(shouldCapture({enabled: false, engineReady: true}), false);
    assert.equal(shouldCapture({enabled: true, engineReady: false}), false);
    assert.equal(shouldCapture({enabled: undefined, engineReady: true}), false);
    assert.equal(shouldCapture({}), false);
    assert.equal(shouldCapture({enabled: true, engineReady: true}), true);
});

test('the provider vocabulary matches the contract', () => {
    assert.equal(CLIPBOARD_PROVIDER, 'clipboard');
    assert.equal(DISABLED_RESULT_ID, 'clip:disabled');
});

test('a push payload is the text and its timestamp', () => {
    assert.deepEqual(pushPayload('hello', 1_700_000_000_000), {
        text: 'hello',
        timestamp: 1_700_000_000_000,
    });
});

test('there is no payload for an empty or non-string clipboard', () => {
    assert.equal(pushPayload('', 1_000), null);
    assert.equal(pushPayload(null, 1_000), null);
    assert.equal(pushPayload(undefined, 1_000), null);
    assert.equal(pushPayload({text: 'nope'}, 1_000), null);
});

test('a nonsense timestamp becomes 0 rather than an invalid D-Bus value', () => {
    assert.equal(pushPayload('text', -5).timestamp, 0);
    assert.equal(pushPayload('text', Number.NaN).timestamp, 0);
    assert.equal(pushPayload('text', Number.POSITIVE_INFINITY).timestamp, 0);
    assert.equal(pushPayload('text', 'nonsense').timestamp, 0);
    assert.equal(pushPayload('text', 12.7).timestamp, 12);
});

test('the first sample records what is on the clipboard', () => {
    const sampler = createClipboardSampler();
    assert.deepEqual(sampler.sample('copied earlier', 1_000), {
        text: 'copied earlier',
        timestamp: 1_000,
    });
});

test('an unchanged clipboard is never pushed twice', () => {
    const sampler = createClipboardSampler();

    assert.deepEqual(sampler.sample('same', 1_000), {text: 'same', timestamp: 1_000});
    assert.equal(sampler.sample('same', 2_000), null);
    assert.equal(sampler.sample('same', 3_000), null);

    assert.deepEqual(sampler.sample('different', 4_000), {
        text: 'different',
        timestamp: 4_000,
    });
    assert.equal(sampler.sample('different', 5_000), null);

    // Copying the first text again is a change again, and the daemon de-duplicates it.
    assert.deepEqual(sampler.sample('same', 6_000), {text: 'same', timestamp: 6_000});
});

test('an empty clipboard is not a copy, and it does not re-arm the last text', () => {
    const sampler = createClipboardSampler();

    sampler.sample('text', 1_000);
    assert.equal(sampler.sample('', 2_000), null, 'an image owner reads back as empty');
    assert.equal(sampler.sample('', 3_000), null);
    assert.equal(sampler.sample('', 4_000), null);
    assert.equal(sampler.last, '');
});

test('a reset makes the next sample count as new', () => {
    const sampler = createClipboardSampler();

    sampler.sample('text', 1_000);
    assert.equal(sampler.sample('text', 2_000), null);

    sampler.reset();
    assert.deepEqual(sampler.sample('text', 3_000), {text: 'text', timestamp: 3_000});
});

test('the sampler reads the clock when no timestamp is given', () => {
    let now = 42;
    const sampler = createClipboardSampler({now: () => now});

    assert.deepEqual(sampler.sample('tick'), {text: 'tick', timestamp: 42});
    now = 99;
    assert.deepEqual(sampler.sample('tock'), {text: 'tock', timestamp: 99});
});
