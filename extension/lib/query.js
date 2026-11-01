// Query shaping: what the shell sends the daemon for a typed query, and the modifier that turns
// "open" into "open a second window". Pure JS, so the shell's key handling is testable by number.

import {PROVIDER_IDS} from './protocol.js';

// Clutter.ModifierType.CONTROL_MASK. Spelled out as a number so nothing here needs a shell:
// Clutter's modifier type is a flag set, SHIFT=1<<0, LOCK=1<<1, CONTROL=1<<2, MOD1=1<<3, …
export const SECOND_WINDOW_MODIFIER = 1 << 2;

export function hasSecondWindowModifier(modifierMask) {
    return ((modifierMask ?? 0) & SECOND_WINDOW_MODIFIER) !== 0;
}

/**
 * The `Query` arguments for `text`, or an empty request when nothing was typed: the shell only
 * searches once there is something to search for.
 */
export function buildRequest(text, enabledProviders = PROVIDER_IDS) {
    const trimmed = (text ?? '').trim();
    if (trimmed === '')
        return {text: '', enabled: []};

    const enabled = new Set(enabledProviders ?? []);
    return {text: trimmed, enabled: PROVIDER_IDS.filter(id => enabled.has(id))};
}
