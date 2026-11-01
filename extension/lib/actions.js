// System actions (lock, suspend, log out) and setting toggles (Wi-Fi, Bluetooth, Do Not Disturb,
// appearance), as contract §3 results for `Query.options.injected`: the daemon re-scores and ranks
// them with its own providers, and the shell performs them because only it can.
//
// The shell supplies live state as plain objects (`{verb, available, reason}` / `{key, on, …}`);
// this module owns the presentation and the confirmation rule, so both are testable without a
// shell. The shell-side gathering lives in `extension/system.js`.
//
// Pure JS: no `gi://` imports, so `node --test` loads it.

import {ID_PREFIXES} from './protocol.js';
import {matchQuality, score} from './scoring.js';

export const ACTION_PROVIDER = 'actions';
export const SETTINGS_PROVIDER = 'settings';

/** The verbs the shell knows how to perform. */
export const ACTION_VERBS = ['lock', 'suspend', 'logout'];
/** The toggles the shell knows how to read and flip. */
export const TOGGLE_KEYS = ['wifi', 'bluetooth', 'dnd', 'appearance'];

// Action ids inside a row: `run` acts at once, `confirm` is the destructive one the shell must ask
// about before performing (§7b). Both handlers are `shell`: only the shell process can lock the
// screen, suspend, end the session or flip a system switch.
const RUN = 'run';
const CONFIRM = 'confirm';
const TOGGLE = 'toggle';
const SHELL = 'shell';
const DEFAULT_REASON = 'not available in this session';

/**
 * The three actions and their wording. `destructive: true` (log out) makes the row ask for a
 * confirmation; the other two act immediately. `aliases` are what a user types besides the title.
 */
export const DEFAULT_ACTIONS = [
    {
        verb: 'lock',
        title: 'Lock Screen',
        subtitle: 'Lock the screen now',
        icon: 'system-lock-screen-symbolic',
        aliases: ['lock screen', 'lock'],
        destructive: false,
        preview: 'Locks the screen immediately.',
    },
    {
        verb: 'suspend',
        title: 'Suspend',
        subtitle: 'Suspend the computer',
        icon: 'media-playback-pause-symbolic',
        aliases: ['sleep'],
        destructive: false,
        preview: 'Suspends the computer; it wakes up on a key press.',
    },
    {
        verb: 'logout',
        title: 'Log Out',
        subtitle: 'End the session — asks to confirm',
        icon: 'system-log-out-symbolic',
        aliases: ['logout', 'log out', 'sign off', 'end session'],
        destructive: true,
        preview: 'Ends your session. Asks for confirmation first.',
    },
];

/**
 * The four toggles and their wording — everything except the live state, which only the shell has.
 * `dnd` is Do Not Disturb (`on` means notifications are silenced) and `appearance` is dark mode.
 */
export const TOGGLE_DEFAULTS = {
    wifi: {
        title: 'Wi-Fi',
        icon: 'network-wireless-symbolic',
        aliases: ['wifi', 'wi-fi', 'wireless', 'wlan'],
    },
    bluetooth: {
        title: 'Bluetooth',
        icon: 'bluetooth-active-symbolic',
        aliases: ['bt', 'blue tooth'],
    },
    dnd: {
        title: 'Do Not Disturb',
        icon: 'notifications-disabled-symbolic',
        aliases: ['dnd', 'do not disturb', 'notifications', 'banners'],
    },
    appearance: {
        title: 'Dark Mode',
        icon: 'dark-mode-symbolic',
        aliases: ['appearance', 'theme', 'color scheme', 'dark', 'light'],
    },
};

export function actionResultId(verb) {
    return `${ID_PREFIXES[ACTION_PROVIDER]}${verb}`;
}

export function actionVerbFromResultId(resultId) {
    return idAfterPrefix(resultId, ID_PREFIXES[ACTION_PROVIDER]);
}

export function settingResultId(key) {
    return `${ID_PREFIXES[SETTINGS_PROVIDER]}${key}`;
}

export function settingKeyFromResultId(resultId) {
    return idAfterPrefix(resultId, ID_PREFIXES[SETTINGS_PROVIDER]);
}

/**
 * Matching actions as result rows, best first. `actions` entries are `{verb, available?, reason?}`
 * plus optional wording overrides; an entry that does not match the query on its title or one of
 * its aliases is dropped, and an empty query matches nothing.
 */
export function actionCandidates(query, {actions = DEFAULT_ACTIONS} = {}) {
    const needle = trimmed(query);
    if (needle === '')
        return [];

    const results = [];
    for (const entry of actions ?? []) {
        const action = {...defaultAction(entry?.verb), ...entry};

        if (!action.verb || matchQualityOf(action, needle) === 0)
            continue;

        results.push(actionRow(action, needle));
    }

    return results.sort(compareCandidates);
}

/**
 * Matching toggles as result rows, best first. `toggles` entries are `{key, on, available?, reason?,
 * detail?}`; the title, icon and aliases come from `TOGGLE_DEFAULTS` unless the shell overrides
 * them. A toggle with no live state reads `Off` rather than `undefined`, and one whose source is
 * absent is a disabled row with the reason in its subtitle instead of a thrown error.
 */
export function toggleCandidates(query, {toggles = []} = {}) {
    const needle = trimmed(query);
    if (needle === '')
        return [];

    const results = [];
    for (const entry of toggles ?? []) {
        const key = entry?.key;
        if (!key)
            continue;

        const toggle = {...TOGGLE_DEFAULTS[key], ...entry, key};
        if (matchQualityOf(toggle, needle) === 0)
            continue;

        results.push(toggleRow(toggle, needle));
    }

    return results.sort(compareCandidates);
}

function actionRow(action, query) {
    const title = action.title ?? action.verb;

    if (action.available === false)
        return disabledRow(action, ACTION_PROVIDER, actionResultId(action.verb), query);

    const destructive = action.destructive === true;
    const preview = {kind: 'text', text: action.preview ?? action.subtitle ?? ''};
    if (destructive)
        preview.destructive = true;

    return {
        id: actionResultId(action.verb),
        provider: ACTION_PROVIDER,
        title,
        subtitle: action.subtitle ?? '',
        icon: action.icon ?? '',
        score: rowScore(ACTION_PROVIDER, action, title, query),
        actions: [{
            id: destructive ? CONFIRM : RUN,
            label: destructive ? (action.confirmLabel ?? title) : title,
            handler: SHELL,
        }],
        preview,
    };
}

function toggleRow(toggle, query) {
    const title = toggle.title ?? toggle.key;

    if (toggle.available === false)
        return disabledRow(toggle, SETTINGS_PROVIDER, settingResultId(toggle.key), query);

    const on = toggle.on === true;
    const detail = toggle.detail ? ` — ${toggle.detail}` : '';

    return {
        id: settingResultId(toggle.key),
        provider: SETTINGS_PROVIDER,
        title,
        subtitle: `${on ? 'On' : 'Off'}${detail}`,
        icon: toggle.icon ?? '',
        score: rowScore(SETTINGS_PROVIDER, toggle, title, query),
        actions: [{id: TOGGLE, label: on ? 'Turn Off' : 'Turn On', handler: SHELL}],
        preview: {kind: 'text', text: `${title} is ${on ? 'on' : 'off'}${detail}.`},
    };
}

// A candidate whose live source is missing still shows up, saying why and offering nothing to run.
function disabledRow(candidate, provider, id, query) {
    const title = candidate.title ?? candidate.verb ?? candidate.key;
    const reason = candidate.reason || DEFAULT_REASON;

    return {
        id,
        provider,
        title,
        subtitle: `Unavailable — ${reason}`,
        icon: candidate.icon ?? '',
        score: rowScore(provider, candidate, title, query),
        actions: [],
        preview: {kind: 'text', text: `${title} is unavailable: ${reason}`, disabled: true},
    };
}

// The shared scorer (§5) works on the title; a synonym match is worth what the synonym is worth,
// so the row scores as well as if the user had typed the title that synonym stands for.
function rowScore(provider, candidate, title, query) {
    const onTitle = matchQuality(title, query);
    return score({provider, title}, query) + (matchQualityOf(candidate, query) - onTitle);
}

function matchQualityOf(candidate, query) {
    const title = candidate.title ?? candidate.verb ?? candidate.key ?? '';
    return Math.max(
        matchQuality(title, query),
        ...(candidate.aliases ?? []).map(alias => matchQuality(alias, query)),
    );
}

function defaultAction(verb) {
    return DEFAULT_ACTIONS.find(action => action.verb === verb) ?? {};
}

function idAfterPrefix(resultId, prefix) {
    return typeof resultId === 'string' && resultId.startsWith(prefix)
        ? resultId.slice(prefix.length)
        : null;
}

function trimmed(query) {
    return (query ?? '').trim();
}

function compareCandidates(a, b) {
    return b.score - a.score
        || compareText(a.title, b.title)
        || compareText(a.id, b.id);
}

function compareText(a, b) {
    return a < b ? -1 : a > b ? 1 : 0;
}
