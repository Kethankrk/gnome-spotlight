import {test} from 'node:test';
import assert from 'node:assert/strict';

import {pickMonitor, centeredOrigin} from '../lib/monitors.js';

// A plausible three-head desk: a laptop at the origin, a big display to its right and a
// portrait monitor to its left, so negative coordinates are exercised too.
const LAPTOP = {x: 0, y: 0, width: 1920, height: 1080};
const RIGHT = {x: 1920, y: 0, width: 2560, height: 1440};
const LEFT = {x: -1280, y: 0, width: 1280, height: 1024};

const NORMAL = [LAPTOP, RIGHT];

test('the pointer picks the monitor that contains it', () => {
    assert.equal(pickMonitor(NORMAL, {x: 100, y: 100}, {primary: LAPTOP}), 0);
    assert.equal(pickMonitor(NORMAL, {x: 2000, y: 100}, {primary: LAPTOP}), 1);
});

test('a monitor left of the origin is found by its negative coordinates', () => {
    const monitors = [LEFT, LAPTOP];
    assert.equal(pickMonitor(monitors, {x: -100, y: 100}, {primary: LAPTOP}), 0);
    assert.equal(pickMonitor(monitors, {x: 100, y: 100}, {primary: LAPTOP}), 1);
});

test('the top-left corner of a monitor belongs to it (bounds are inclusive)', () => {
    assert.equal(pickMonitor(NORMAL, {x: LAPTOP.x, y: LAPTOP.y}, {primary: RIGHT}), 0);
    assert.equal(pickMonitor(NORMAL, {x: RIGHT.x, y: RIGHT.y}, {primary: LAPTOP}), 1);
});

test('the right/bottom edge belongs to the next monitor, not the one it closes', () => {
    // x == laptop.x + laptop.width is the first pixel of the monitor to the right.
    assert.equal(pickMonitor(NORMAL, {x: 1920, y: 540}, {primary: LAPTOP}), 1);
    // The row below a monitor is outside it, and the fallback is the primary, not the neighbour.
    const stacked = [LAPTOP, {x: 0, y: 1080, width: 1920, height: 1080}];
    assert.equal(pickMonitor(stacked, {x: 500, y: 1080}, {primary: LAPTOP}), 1);
    assert.equal(pickMonitor(stacked, {x: 500, y: 1079.9}, {primary: RIGHT}), 0);
});

test('a pointer that is off every monitor falls back to the primary', () => {
    // The dead zone of an L-shaped desk, and a point past the far edge.
    const monitors = [{x: 0, y: 0, width: 1000, height: 800}, RIGHT];
    assert.equal(pickMonitor(monitors, {x: 1500, y: 100}, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, {x: 9999, y: 9999}, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, {x: 9999, y: 9999}, {primary: LAPTOP}), 0);
});

test('a null or unusable pointer falls back to the primary', () => {
    assert.equal(pickMonitor(NORMAL, null, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, undefined, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, {}, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, {x: 'nope', y: 0}, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, {x: NaN, y: 0}, {primary: RIGHT}), 1);
});

test('without a primary the first monitor is the fallback', () => {
    for (const primary of [null, undefined]) {
        assert.equal(pickMonitor(NORMAL, null, {primary}), 0);
        assert.equal(pickMonitor(NORMAL, {x: 9999, y: 9999}, {primary}), 0);
    }
    assert.equal(pickMonitor(NORMAL, null), 0);
    assert.equal(pickMonitor(NORMAL, null, {}), 0);
});

test('the primary may be named by index or by monitor object', () => {
    assert.equal(pickMonitor(NORMAL, null, {primary: 1}), 1);
    assert.equal(pickMonitor(NORMAL, null, {primary: 0}), 0);
    assert.equal(pickMonitor(NORMAL, null, {primary: RIGHT}), 1);
    assert.equal(pickMonitor(NORMAL, null, {primary: {x: 1920, y: 0, width: 2560, height: 1440}}), 1);
});

test('an unusable primary falls back to the first monitor', () => {
    assert.equal(pickMonitor(NORMAL, null, {primary: 5}), 0);
    assert.equal(pickMonitor(NORMAL, null, {primary: -1}), 0);
    assert.equal(pickMonitor(NORMAL, null, {primary: 1.5}), 0);
    assert.equal(pickMonitor(NORMAL, null, {primary: '1'}), 0);
    assert.equal(pickMonitor(NORMAL, null, {primary: {x: 7000, y: 0, width: 1, height: 1}}), 0);
    assert.equal(pickMonitor(NORMAL, null, {primary: {}}), 0);
});

test('the pointer wins over the primary, always', () => {
    assert.equal(pickMonitor(NORMAL, {x: 10, y: 10}, {primary: 1}), 0);
    assert.equal(pickMonitor(NORMAL, {x: 3000, y: 300}, {primary: 0}), 1);
});

test('overlapping monitors resolve to the first one that contains the pointer', () => {
    const monitors = [{x: 0, y: 0, width: 1920, height: 1080}, {x: 1000, y: 0, width: 1920, height: 1080}];
    assert.equal(pickMonitor(monitors, {x: 1500, y: 100}, {primary: 1}), 0);
    assert.equal(pickMonitor(monitors, {x: 2100, y: 100}, {primary: 0}), 1);
});

test('fractional pointer positions are contained normally', () => {
    assert.equal(pickMonitor(NORMAL, {x: 1919.5, y: 1079.5}, {primary: RIGHT}), 0);
});

test('an empty monitor list has no monitor to pick', () => {
    assert.equal(pickMonitor([], {x: 0, y: 0}, {primary: LAPTOP}), -1);
    assert.equal(pickMonitor([], null), -1);
    assert.equal(pickMonitor(null, {x: 0, y: 0}), -1);
    assert.equal(pickMonitor(undefined, null), -1);
});

test('the pick is a plain integer index into the given list', () => {
    const picked = pickMonitor(NORMAL, {x: 2000, y: 10}, {primary: LAPTOP});
    assert.equal(Number.isInteger(picked), true);
    assert.equal(NORMAL[picked], RIGHT);
});

test('centeredOrigin centres a card inside a monitor at the origin', () => {
    assert.deepEqual(centeredOrigin(LAPTOP, {width: 640, height: 400}), {x: 640, y: 340});
});

test('centeredOrigin rounds an odd leftover to a whole pixel', () => {
    // (1920 - 641) / 2 = 639.5 and (1080 - 401) / 2 = 339.5
    assert.deepEqual(centeredOrigin(LAPTOP, {width: 641, height: 401}), {x: 640, y: 340});
});

test('centeredOrigin is relative to the monitor, not the desktop origin', () => {
    assert.deepEqual(centeredOrigin(RIGHT, {width: 640, height: 400}), {x: 2880, y: 520});
    assert.deepEqual(centeredOrigin(LEFT, {width: 640, height: 400}), {x: -960, y: 312});
});

test('centeredOrigin lets a card bigger than its monitor hang outside it', () => {
    assert.deepEqual(centeredOrigin({x: 0, y: 0, width: 800, height: 600}, {width: 1000, height: 700}),
        {x: -100, y: -50});
});

test('centeredOrigin degrades instead of throwing on missing or junk input', () => {
    assert.deepEqual(centeredOrigin(null, {width: 10, height: 10}), {x: 0, y: 0});
    assert.deepEqual(centeredOrigin(undefined), {x: 0, y: 0});
    assert.deepEqual(centeredOrigin({x: NaN, y: 0, width: 1, height: 1}, {width: 0, height: 0}),
        {x: 0, y: 0});
    // A missing size is a zero-sized card, which centres at the monitor's midpoint.
    assert.deepEqual(centeredOrigin({x: 0, y: 0, width: 100, height: 100}), {x: 50, y: 50});
    assert.deepEqual(centeredOrigin({x: 10, y: 20, width: 100, height: 100}, {}), {x: 60, y: 70});
});

test('centeredOrigin returns whole pixels, never -0', () => {
    const origin = centeredOrigin({x: 0, y: 0, width: 100, height: 100}, {width: 101, height: 101});
    assert.equal(Object.is(origin.x, -0), false);
    assert.equal(Object.is(origin.y, -0), false);
    assert.equal(Number.isInteger(origin.x), true);
    assert.equal(Number.isInteger(origin.y), true);
});
