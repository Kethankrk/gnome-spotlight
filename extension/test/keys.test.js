import {test} from 'node:test';
import assert from 'node:assert/strict';

import {intentFor, KEYS, DEFAULT_PAGE_SIZE} from '../lib/keys.js';

test('the keyvals are the Gdk/Clutter symbol numbers', () => {
    assert.equal(KEYS.Up, 0xff52);
    assert.equal(KEYS.Down, 0xff54);
    assert.equal(KEYS.Home, 0xff50);
    assert.equal(KEYS.End, 0xff57);
    assert.equal(KEYS.PageUp, 0xff55);
    assert.equal(KEYS.PageDown, 0xff56);
    assert.equal(KEYS.Return, 0xff0d);
    assert.equal(KEYS.KpEnter, 0xff8d);
    assert.equal(KEYS.Tab, 0xff09);
    assert.equal(KEYS.IsoLeftTab, 0xfe20);
});

test('navigation keys map to intents by number', () => {
    assert.equal(intentFor(0xff54), 'next');
    assert.equal(intentFor(0xff52), 'prev');
    assert.equal(intentFor(0xff50), 'first');
    assert.equal(intentFor(0xff57), 'last');
    assert.equal(intentFor(0xff56), 'page-down');
    assert.equal(intentFor(0xff55), 'page-up');
    assert.equal(intentFor(0xff0d), 'activate');
});

test('the numeric keypad enter activates too', () => {
    assert.equal(intentFor(KEYS.KpEnter), 'activate');
});

test('Tab walks the list and Shift+Tab walks back', () => {
    assert.equal(intentFor(KEYS.Tab), 'next');
    assert.equal(intentFor(KEYS.IsoLeftTab), 'prev');
    assert.equal(intentFor(KEYS.Tab, {shift: true}), 'prev');
});

test('every other key is not a navigation intent', () => {
    for (const keyval of ['a'.charCodeAt(0), 0xff1b, 0xffe1, 0, 0x20])
        assert.equal(intentFor(keyval), null);
    assert.equal(intentFor(undefined), null);
});

test('the page size is reported so the caller shares one default', () => {
    assert.equal(intentFor(KEYS.PageDown, {pageSize: 4}), 'page-down');
    assert.ok(DEFAULT_PAGE_SIZE >= 1);
});
