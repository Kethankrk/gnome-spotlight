import {test} from 'node:test';
import assert from 'node:assert/strict';

import {
    ACTION_PROVIDER,
    SETTINGS_PROVIDER,
    ACTION_VERBS,
    TOGGLE_KEYS,
    DEFAULT_ACTIONS,
    TOGGLE_DEFAULTS,
    actionCandidates,
    actionResultId,
    actionVerbFromResultId,
    settingKeyFromResultId,
    settingResultId,
    toggleCandidates,
} from '../lib/actions.js';
import {matchQuality} from '../lib/scoring.js';

/** All four toggles, off and available — the shape the shell hands in. */
const ALL_OFF = TOGGLE_KEYS.map(key => ({key, on: false}));

test('an empty query produces nothing, for actions and toggles alike', () => {
    for (const query of ['', '   ', '\t\n', undefined, null]) {
        assert.deepEqual(actionCandidates(query), [], `actions: ${JSON.stringify(query)}`);
        assert.deepEqual(toggleCandidates(query, {toggles: ALL_OFF}), [], `toggles: ${JSON.stringify(query)}`);
    }
});

test('a query that matches nothing produces nothing', () => {
    assert.deepEqual(actionCandidates('zzzz'), []);
    assert.deepEqual(toggleCandidates('zzzz', {toggles: ALL_OFF}), []);
});

test('a query matches the title, case-insensitively', () => {
    assert.deepEqual(actionCandidates('SUSPEND').map(r => r.id), ['action:suspend']);
    assert.deepEqual(toggleCandidates('bLuEtOoTh', {toggles: ALL_OFF}).map(r => r.id), ['setting:bluetooth']);
    assert.deepEqual(actionCandidates('sus').map(r => r.id), ['action:suspend']);
});

test('without state from the shell there are no toggles to show', () => {
    assert.deepEqual(toggleCandidates('wifi', {toggles: []}), []);
    assert.deepEqual(toggleCandidates('wifi'), []);
    assert.deepEqual(actionCandidates('suspend', {actions: []}), []);
});

test('a lock action carries the contract result shape', () => {
    const [lock] = actionCandidates('lock screen');

    assert.deepEqual(lock, {
        id: 'action:lock',
        provider: 'actions',
        title: 'Lock Screen',
        subtitle: 'Lock the screen now',
        icon: 'system-lock-screen-symbolic',
        score: 0.25 + 1.0,
        actions: [{id: 'run', label: 'Lock Screen', handler: 'shell'}],
        preview: {kind: 'text', text: 'Locks the screen immediately.'},
    });
});

test('a suspend action carries the contract result shape', () => {
    const [suspend] = actionCandidates('suspend');

    assert.deepEqual(suspend, {
        id: 'action:suspend',
        provider: 'actions',
        title: 'Suspend',
        subtitle: 'Suspend the computer',
        icon: 'media-playback-pause-symbolic',
        score: 0.25 + 1.0,
        actions: [{id: 'run', label: 'Suspend', handler: 'shell'}],
        preview: {kind: 'text', text: 'Suspends the computer; it wakes up on a key press.'},
    });
});

test('lock and suspend act immediately, log out asks to confirm', () => {
    for (const query of ['lock', 'suspend']) {
        const [result] = actionCandidates(query);
        assert.deepEqual(result.actions.map(a => ({id: a.id, handler: a.handler})),
            [{id: 'run', handler: 'shell'}], `${query} must act immediately`);
        assert.equal(result.preview.destructive, undefined, `${query} is not destructive`);
    }

    const [logout] = actionCandidates('logout');
    assert.equal(logout.id, 'action:logout');
    assert.equal(logout.provider, 'actions');
    assert.equal(logout.title, 'Log Out');
    assert.deepEqual(logout.actions, [{id: 'confirm', label: 'Log Out', handler: 'shell'}]);
    assert.equal(logout.preview.destructive, true);
    assert.equal(logout.preview.kind, 'text');
    assert.match(logout.subtitle, /confirm/i);
});

test('an action matches through its synonyms as well as its title', () => {
    assert.deepEqual(actionCandidates('log out').map(r => r.id), ['action:logout']);
    assert.deepEqual(actionCandidates('logout').map(r => r.id), ['action:logout']);
    assert.deepEqual(actionCandidates('sign off').map(r => r.id), ['action:logout']);
    assert.deepEqual(actionCandidates('sleep').map(r => r.id), ['action:suspend']);

    // A synonym match ranks as well as a title match: the verb is what the user typed.
    assert.equal(actionCandidates('logout')[0].score, 0.25 + 1.0);
    assert.equal(actionCandidates('sleep')[0].score, 0.25 + 1.0);
});

test('a toggle matches through its synonyms as well as its title', () => {
    assert.deepEqual(toggleCandidates('wifi', {toggles: ALL_OFF}).map(r => r.id), ['setting:wifi']);
    assert.deepEqual(toggleCandidates('wireless', {toggles: ALL_OFF}).map(r => r.id), ['setting:wifi']);
    assert.deepEqual(toggleCandidates('dnd', {toggles: ALL_OFF}).map(r => r.id), ['setting:dnd']);
    assert.deepEqual(toggleCandidates('notifications', {toggles: ALL_OFF}).map(r => r.id), ['setting:dnd']);
    assert.deepEqual(toggleCandidates('dark', {toggles: ALL_OFF}).map(r => r.id), ['setting:appearance']);
    assert.deepEqual(toggleCandidates('theme', {toggles: ALL_OFF}).map(r => r.id), ['setting:appearance']);

    assert.equal(toggleCandidates('wifi', {toggles: ALL_OFF})[0].score, 0.25 + 1.0);
});

test('every toggle key has a default presentation that matches its own key', () => {
    for (const key of TOGGLE_KEYS) {
        const [result] = toggleCandidates(key, {toggles: ALL_OFF});
        assert.equal(result.id, settingResultId(key), `${key} is not reachable by its own key`);
        assert.equal(result.provider, SETTINGS_PROVIDER);
        assert.equal(result.title, TOGGLE_DEFAULTS[key].title);
        assert.equal(result.icon, TOGGLE_DEFAULTS[key].icon);
        assert.ok(result.subtitle.length > 0);
    }
});

test('every action verb has a default presentation that matches its own verb', () => {
    for (const verb of ACTION_VERBS) {
        const [result] = actionCandidates(verb);
        assert.equal(result.id, actionResultId(verb), `${verb} is not reachable by its own verb`);
        assert.equal(result.provider, ACTION_PROVIDER);
        assert.equal(result.title, DEFAULT_ACTIONS.find(a => a.verb === verb).title);
        assert.equal(result.icon, DEFAULT_ACTIONS.find(a => a.verb === verb).icon);
    }
});

test('an on toggle shows On in the subtitle and offers to turn it off', () => {
    const [wifi] = toggleCandidates('wifi', {toggles: [{key: 'wifi', on: true}]});

    assert.deepEqual(wifi, {
        id: 'setting:wifi',
        provider: 'settings',
        title: 'Wi-Fi',
        subtitle: 'On',
        icon: 'network-wireless-symbolic',
        score: 0.25 + 1.0,
        actions: [{id: 'toggle', label: 'Turn Off', handler: 'shell'}],
        preview: {kind: 'text', text: 'Wi-Fi is on.'},
    });
});

test('an off toggle shows Off in the subtitle and offers to turn it on', () => {
    const [dnd] = toggleCandidates('dnd', {toggles: [{key: 'dnd', on: false}]});

    assert.equal(dnd.subtitle, 'Off');
    assert.deepEqual(dnd.actions, [{id: 'toggle', label: 'Turn On', handler: 'shell'}]);
    assert.deepEqual(dnd.preview, {kind: 'text', text: 'Do Not Disturb is off.'});
});

test('a toggle with no live state reads Off rather than undefined', () => {
    const [bt] = toggleCandidates('bluetooth', {toggles: [{key: 'bluetooth'}]});
    assert.equal(bt.subtitle, 'Off');
    assert.match(bt.preview.text, /is off/);
});

test('a toggle in a mixed state shows both the state and the reason', () => {
    const [wifi] = toggleCandidates('wifi', {
        toggles: [{key: 'wifi', on: true, detail: 'the hardware switch is off'}],
    });

    assert.equal(wifi.subtitle, 'On — the hardware switch is off');
    assert.match(wifi.preview.text, /Wi-Fi is on — the hardware switch is off/);
});

test('an unavailable toggle is a disabled row, not a thrown error', () => {
    const [bt] = toggleCandidates('bluetooth', {
        toggles: [{key: 'bluetooth', on: false, available: false, reason: 'the Bluetooth service is not running'}],
    });

    assert.deepEqual(bt.actions, [], 'a disabled row has nothing to activate');
    assert.equal(bt.subtitle, 'Unavailable — the Bluetooth service is not running');
    assert.deepEqual(bt.preview, {
        kind: 'text',
        text: 'Bluetooth is unavailable: the Bluetooth service is not running',
        disabled: true,
    });
});

test('an unavailable toggle without a reason still says something', () => {
    const [wifi] = toggleCandidates('wifi', {toggles: [{key: 'wifi', available: false}]});
    assert.deepEqual(wifi.actions, []);
    assert.equal(wifi.subtitle, 'Unavailable — not available in this session');
    assert.equal(wifi.preview.disabled, true);
});

test('an unavailable action is a disabled row too', () => {
    const [suspend] = actionCandidates('suspend', {
        actions: [{verb: 'suspend', available: false, reason: 'suspend is not available in this session'}],
    });

    assert.equal(suspend.provider, ACTION_PROVIDER);
    assert.equal(suspend.title, 'Suspend');
    assert.deepEqual(suspend.actions, []);
    assert.equal(suspend.subtitle, 'Unavailable — suspend is not available in this session');
    assert.deepEqual(suspend.preview, {
        kind: 'text',
        text: 'Suspend is unavailable: suspend is not available in this session',
        disabled: true,
    });
});

test('an action supplied by the shell can override the default wording', () => {
    const [lock] = actionCandidates('lock screen', {
        actions: [{verb: 'lock', title: 'Lägg lås', subtitle: 'Lås skärmen', icon: 'changed', reason: ''}],
    });

    assert.equal(lock.title, 'Lägg lås');
    assert.equal(lock.subtitle, 'Lås skärmen');
    assert.equal(lock.icon, 'changed');
    assert.equal(lock.id, 'action:lock');
});

test('actions are sorted by score, ties by title', () => {
    // 'l' is a prefix of both "Lock Screen" and "Log Out" and a substring of "sleep", so lock and
    // log out tie above suspend and the tie breaks on the title.
    assert.deepEqual(actionCandidates('l').map(r => r.id),
        ['action:lock', 'action:logout', 'action:suspend']);
    assert.deepEqual(actionCandidates('suspend')[0].id, 'action:suspend');
});

test('toggles are sorted by score, ties by title', () => {
    // 'o' is a substring of Bluetooth, Dark Mode and Do Not Disturb, so the three tie and the
    // order is the title order.
    assert.deepEqual(toggleCandidates('o', {toggles: ALL_OFF}).map(r => r.id),
        ['setting:bluetooth', 'setting:appearance', 'setting:dnd']);
});

test('scores come from the shared scorer: base weight plus the best match', () => {
    const [lock] = actionCandidates('lock');
    assert.equal(lock.score, 0.25 + matchQuality('Lock Screen', 'lock')
        + (1.0 - matchQuality('Lock Screen', 'lock')), 'the exact synonym match is worth 1.0');
    assert.equal(lock.score, 1.25);

    const [appearance] = toggleCandidates('theme', {toggles: ALL_OFF});
    assert.equal(appearance.score, 0.25 + 1.0);
});

test('result ids round-trip through the action: and setting: prefixes', () => {
    assert.equal(actionResultId('lock'), 'action:lock');
    assert.equal(actionVerbFromResultId('action:lock'), 'lock');
    assert.equal(actionVerbFromResultId('setting:wifi'), null);
    assert.equal(actionVerbFromResultId(undefined), null);

    assert.equal(settingResultId('dnd'), 'setting:dnd');
    assert.equal(settingKeyFromResultId('setting:dnd'), 'dnd');
    assert.equal(settingKeyFromResultId('action:lock'), null);
    assert.equal(settingKeyFromResultId(null), null);
});

test('the prefixes are the ones the contract pins', () => {
    assert.equal(ACTION_PROVIDER, 'actions');
    assert.equal(SETTINGS_PROVIDER, 'settings');
    assert.deepEqual(ACTION_VERBS, ['lock', 'suspend', 'logout']);
    assert.deepEqual(TOGGLE_KEYS, ['wifi', 'bluetooth', 'dnd', 'appearance']);
});
