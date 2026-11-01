import {test} from 'node:test';
import assert from 'node:assert/strict';

import {score, matchQuality, recencyBonus, usageBonus, USAGE_CAP} from '../lib/scoring.js';

const NOW = 1_000_000_000_000;

test('match quality ranks exact over prefix over word-start over substring over fuzzy', () => {
    assert.equal(matchQuality('Terminal', 'terminal'), 1.0);
    assert.equal(matchQuality('Terminal Emulator', 'term'), 0.8);
    assert.equal(matchQuality('GNOME Terminal', 'term'), 0.6);
    assert.equal(matchQuality('GNOME Terminal', 'minal'), 0.4);
    assert.equal(matchQuality('Terminal', 'trmnl'), 0.2);
    assert.equal(matchQuality('Terminal', 'zzz'), 0.0);
    assert.equal(matchQuality('Terminal', ''), 0.0);
});

test('an exact application match outranks any file result', () => {
    const inContext = {now: NOW};
    const exactApp = score({provider: 'applications', title: 'Files'}, 'files', inContext);
    const exactFile = score({provider: 'files', title: 'Files'}, 'files', inContext);
    assert.ok(exactApp > exactFile, `${exactApp} must beat ${exactFile}`);
    assert.equal(exactApp, 0.30 + 1.0);
});

test('recency bonus is bounded and decays', () => {
    assert.equal(recencyBonus(null, NOW), 0.0);
    assert.equal(recencyBonus(NOW, NOW), 0.10);
    const old = recencyBonus(NOW - 60 * 24 * 60 * 60 * 1000, NOW);
    assert.ok(old >= 0 && old < 0.10);
});

test('usage bonus is bounded by the cap', () => {
    assert.equal(usageBonus(0), 0.0);
    assert.equal(usageBonus(USAGE_CAP), 0.20);
    assert.equal(usageBonus(USAGE_CAP * 5), 0.20);
});

test('a frequently used loosely matching app can beat an unused exact file', () => {
    const looselyUsed = score(
        {provider: 'applications', title: 'Files', usage: USAGE_CAP},
        'file',
        {now: NOW},
    );
    const exactCold = score({provider: 'files', title: 'file'}, 'file', {now: NOW});
    assert.ok(looselyUsed > exactCold);
});
