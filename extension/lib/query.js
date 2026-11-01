// Query shaping: what the shell sends the daemon for a typed query, and the modifiers that pick
// which of a row's actions a press runs. Pure JS, so the shell's key handling is testable by
// number.
//
// The masks are Clutter.ModifierType bits, spelled out as numbers so nothing here needs a shell:
// SHIFT=1<<0, LOCK=1<<1, CONTROL=1<<2, MOD1 (Alt)=1<<3, …
//
//   plain Enter   the row's primary action (`open`)
//   Control       the secondary one: a second window for an application, `Show in Files` for a file
//   Alt           the copy: a file's path, a calculation's value, a clipboard entry's text
//
// A row that does not offer the action asked for keeps its primary one (lib/activation.js).

import {PROVIDER_IDS} from './protocol.js';

// Clutter.ModifierType.CONTROL_MASK.
export const CONTROL_MASK = 1 << 2;
// Clutter.ModifierType.MOD1_MASK — Alt.
export const ALT_MASK = 1 << 3;

/** Control: open an application in a second window (story 14). */
export const SECOND_WINDOW_MODIFIER = CONTROL_MASK;
/** Control: show a file in the file manager (story 20). The same key, a different meaning. */
export const REVEAL_MODIFIER = CONTROL_MASK;
/** Alt: copy what a result stands for (stories 21, 26, 46). */
export const COPY_MODIFIER = ALT_MASK;

export function hasControlModifier(modifierMask) {
    return ((modifierMask ?? 0) & CONTROL_MASK) !== 0;
}

export function hasSecondWindowModifier(modifierMask) {
    return hasControlModifier(modifierMask);
}

export function hasRevealModifier(modifierMask) {
    return hasControlModifier(modifierMask);
}

export function hasCopyModifier(modifierMask) {
    return ((modifierMask ?? 0) & COPY_MODIFIER) !== 0;
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
