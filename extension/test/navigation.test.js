import {test} from 'node:test';
import assert from 'node:assert/strict';

import {createNavigator, DEFAULT_PAGE_SIZE} from '../lib/navigation.js';

test('a navigator over an empty list stays at -1 and never moves', () => {
    const navigator = createNavigator(0);
    const seen = [];
    navigator.onChange = index => seen.push(index);

    assert.equal(navigator.count, 0);
    assert.equal(navigator.current, -1);
    assert.equal(navigator.next(), -1);
    assert.equal(navigator.prev(), -1);
    assert.equal(navigator.first(), -1);
    assert.equal(navigator.last(), -1);
    assert.equal(navigator.pageDown(5), -1);
    assert.equal(navigator.pageUp(5), -1);
    assert.equal(navigator.select(0), -1);
    assert.deepEqual(seen, []);
});

test('a new navigator starts on the first row', () => {
    const navigator = createNavigator(3);
    assert.equal(navigator.count, 3);
    assert.equal(navigator.current, 0);
});

test('next and prev wrap around', () => {
    const navigator = createNavigator(3);
    assert.equal(navigator.next(), 1);
    assert.equal(navigator.next(), 2);
    assert.equal(navigator.next(), 0, 'next from the last row wraps to the first');
    assert.equal(navigator.prev(), 2, 'prev from the first row wraps to the last');
    assert.equal(navigator.prev(), 1);
});

test('first and last jump to the ends', () => {
    const navigator = createNavigator(5);
    assert.equal(navigator.last(), 4);
    assert.equal(navigator.first(), 0);
});

test('paging clamps at the ends instead of wrapping', () => {
    const navigator = createNavigator(10);
    assert.equal(navigator.pageDown(3), 3);
    assert.equal(navigator.pageDown(3), 6);
    assert.equal(navigator.pageDown(3), 9);
    assert.equal(navigator.pageDown(3), 9, 'paging past the end stays on the last row');
    assert.equal(navigator.pageUp(4), 5);
    assert.equal(navigator.pageUp(4), 1);
    assert.equal(navigator.pageUp(4), 0);
    assert.equal(navigator.pageUp(4), 0, 'paging before the start stays on the first row');
});

test('pageUp and pageDown fall back to the default page size', () => {
    const navigator = createNavigator(20, {pageSize: 5});
    assert.equal(navigator.pageDown(), 5);
    assert.equal(navigator.pageUp(), 0);
    assert.ok(DEFAULT_PAGE_SIZE > 0);
});

test('select only accepts an index inside the list', () => {
    const navigator = createNavigator(3);
    assert.equal(navigator.select(2), 2);
    assert.equal(navigator.select(3), 2, 'out of range is ignored');
    assert.equal(navigator.select(-1), 2);
    assert.equal(navigator.select(1.5), 2);
    assert.equal(navigator.select('1'), 2);
    assert.equal(navigator.current, 2);
});

test('onChange fires only when the selection actually moves', () => {
    const seen = [];
    const navigator = createNavigator(3);
    navigator.onChange = index => seen.push(index);

    navigator.next();
    navigator.next();
    navigator.next();
    navigator.select(0);
    navigator.select(7);
    navigator.first();
    navigator.last();

    assert.deepEqual(seen, [1, 2, 0, 2]);
});
