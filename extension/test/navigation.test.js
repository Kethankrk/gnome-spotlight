import {test} from 'node:test';
import assert from 'node:assert/strict';

import {createNavigator, sectionStarts, DEFAULT_PAGE_SIZE} from '../lib/navigation.js';

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

test('sectionStarts follows the display order: the Top Hit, then each non-empty section', () => {
    assert.deepEqual(sectionStarts({
        topHit: {id: 'app:a'},
        sections: [
            {provider: 'applications', items: [{id: 'app:b'}, {id: 'app:c'}]},
            {provider: 'files', items: [{id: 'file:/x'}]},
        ],
    }), [0, 1, 3]);

    // No Top Hit: the first section starts at the first row.
    assert.deepEqual(sectionStarts({
        topHit: null,
        sections: [{provider: 'files', items: [{id: 'file:/x'}, {id: 'file:/y'}]}],
    }), [0]);

    // A section the view model kept but left empty renders no row, so it is not a stop.
    assert.deepEqual(sectionStarts({
        sections: [
            {provider: 'applications', items: []},
            {provider: 'files', items: [{id: 'file:/x'}]},
        ],
    }), [0]);

    assert.deepEqual(sectionStarts({}), []);
    assert.deepEqual(sectionStarts(), []);
});

test('a navigator with no section information treats every row as its own section', () => {
    const navigator = createNavigator(3);
    assert.deepEqual(navigator.sections, [0, 1, 2]);

    // Tab keeps its list-level meaning, which is what it was before section movement existed.
    assert.equal(navigator.nextSection(), 1);
    assert.equal(navigator.prevSection(), 0);
    assert.equal(navigator.prevSection(), 2, 'and it still wraps');
});

test('nextSection and prevSection land on the first row of a section', () => {
    // Rows: 0 Top Hit | 1,2 Applications | 3,4 Files | 5 Recent
    const navigator = createNavigator(6, {sections: [0, 1, 3, 5]});
    assert.deepEqual(navigator.sections, [0, 1, 3, 5]);
    assert.equal(navigator.current, 0);

    assert.equal(navigator.nextSection(), 1);
    assert.equal(navigator.nextSection(), 3);
    assert.equal(navigator.nextSection(), 5);
    assert.equal(navigator.nextSection(), 0, 'the last section wraps to the Top Hit');

    assert.equal(navigator.prevSection(), 5, 'and back');
    assert.equal(navigator.prevSection(), 3);
    assert.equal(navigator.prevSection(), 1);
    assert.equal(navigator.prevSection(), 0);
});

test('section movement leaves the section you are inside, not the row you are on', () => {
    const navigator = createNavigator(6, {sections: [0, 1, 3]});

    navigator.select(2);           // the second row of the Applications section
    assert.equal(navigator.nextSection(), 3);
    assert.equal(navigator.prevSection(), 1, 'back to the start of the previous section');
});

test('section boundaries are normalised: sorted, unique, inside the list, always starting at 0', () => {
    const navigator = createNavigator(4, {sections: [2, 2, -1, 9, 1.5, '1', 3]});
    assert.deepEqual(navigator.sections, [0, 2, 3]);
});

test('section movement over an empty list never moves', () => {
    const navigator = createNavigator(0, {sections: [0, 1]});
    const seen = [];
    navigator.onChange = index => seen.push(index);

    assert.deepEqual(navigator.sections, []);
    assert.equal(navigator.nextSection(), -1);
    assert.equal(navigator.prevSection(), -1);
    assert.deepEqual(seen, []);
});

test('section movement reports a change once per move', () => {
    const seen = [];
    const navigator = createNavigator(4, {sections: [0, 2]});
    navigator.onChange = index => seen.push(index);

    navigator.nextSection();
    navigator.nextSection();
    navigator.prevSection();

    assert.deepEqual(seen, [2, 0, 2]);
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
