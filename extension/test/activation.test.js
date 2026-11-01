import {test} from 'node:test';
import assert from 'node:assert/strict';

import {actionForModifiers, activationAction} from '../lib/activation.js';
import {COPY_MODIFIER, REVEAL_MODIFIER, SECOND_WINDOW_MODIFIER} from '../lib/query.js';

const file = () => ({
    id: 'file:/home/me/notes/report.md',
    provider: 'files',
    title: 'report.md',
    actions: [
        {id: 'open', label: 'Open', handler: 'open'},
        {id: 'reveal', label: 'Show in Files', handler: 'reveal'},
        {id: 'copy', label: 'Copy Path', handler: 'copy'},
    ],
});

const clipboard = () => ({
    id: 'clip:0000000000000001',
    provider: 'clipboard',
    title: 'copied text',
    actions: [
        {id: 'copy', label: 'Copy', handler: 'copy'},
        {id: 'delete', label: 'Delete', handler: 'shell'},
    ],
});

test('the modifier mask names the action it asks for', () => {
    assert.equal(actionForModifiers(0), 'open', 'plain Enter is the row\'s primary action');
    assert.equal(actionForModifiers(undefined), 'open');
    assert.equal(actionForModifiers(null), 'open');
    assert.equal(actionForModifiers(REVEAL_MODIFIER), 'reveal');
    assert.equal(actionForModifiers(COPY_MODIFIER), 'copy');
    assert.equal(actionForModifiers(COPY_MODIFIER | 1 << 0), 'copy', 'extra bits do not matter');
});

test('control and alt together ask for the copy', () => {
    assert.equal(actionForModifiers(REVEAL_MODIFIER | COPY_MODIFIER), 'copy');
});

test('the requested action is the row\'s own, not a fabricated one', () => {
    assert.equal(activationAction(file(), 0).id, 'open');
    assert.equal(activationAction(file(), REVEAL_MODIFIER).id, 'reveal');
    assert.equal(activationAction(file(), COPY_MODIFIER).id, 'copy');
    assert.equal(activationAction(clipboard(), COPY_MODIFIER).id, 'copy');
});

test('a row that does not offer the action falls back to its primary one', () => {
    // A recent document offers `open` alone: Ctrl+Enter must still open it rather than do nothing.
    const recent = {
        id: 'recent:/home/me/notes/report.md',
        provider: 'recent',
        title: 'report.md',
        actions: [{id: 'open', label: 'Open', handler: 'open'}],
    };

    assert.equal(activationAction(recent, REVEAL_MODIFIER).id, 'open');
    assert.equal(activationAction(recent, COPY_MODIFIER).id, 'open');
});

test('an action carrying the handler without the id is still found', () => {
    const web = {
        id: 'web:abc',
        provider: 'websearch',
        title: 'Search the web for x',
        actions: [{id: 'open', label: 'Search', handler: 'shell'}],
    };

    assert.equal(activationAction(web, 0).id, 'open');
});

test('a row with nothing to run activates nothing', () => {
    assert.equal(activationAction({id: 'clip:disabled', actions: []}, 0), null);
    assert.equal(activationAction({id: 'action:lock'}, 0), null);
    assert.equal(activationAction(null, 0), null);
});

test('the second-window modifier is the reveal modifier, so applications keep their behaviour', () => {
    assert.equal(SECOND_WINDOW_MODIFIER, REVEAL_MODIFIER);
});
