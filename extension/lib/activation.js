// Which action a key press runs on the selected row (stories 19, 20, 21, 26 and 46): plain Enter
// runs the row's primary action, Control asks for the secondary one — `Show in Files` for a file, a
// second window for an application — and Alt asks for the copy. The row's own `actions` say what it
// can do, so a provider that offers nothing but `open` still opens on any modifier.
//
// Pure JS: no `gi://` imports, so `node --test` loads it.

import {hasCopyModifier, hasRevealModifier} from './query.js';

/** The action id plain Enter runs. */
export const PRIMARY_ACTION = 'open';
/** The action id Control asks for. */
export const SECONDARY_ACTION = 'reveal';
/** The action id Alt asks for. */
export const COPY_ACTION = 'copy';

/**
 * The action id a modifier mask asks for. Alt wins over Control when both are held, so the copy —
 * the least destructive of the three — is what a fat-fingered modifier combination does.
 */
export function actionForModifiers(modifiers = 0) {
    if (hasCopyModifier(modifiers))
        return COPY_ACTION;
    if (hasRevealModifier(modifiers))
        return SECONDARY_ACTION;

    return PRIMARY_ACTION;
}

/**
 * The action object to run on `result` for this key press, or `null` when the row has nothing to
 * run (a disabled row, an entry with no actions). The wanted action is matched by id first and by
 * handler second; a row that does not offer it keeps its primary action, which is what makes
 * Control on a recent document or on the web-search row open it rather than do nothing.
 */
export function activationAction(result, modifiers = 0) {
    const actions = (result?.actions ?? []).filter(action => action);
    if (actions.length === 0)
        return null;

    const wanted = actionForModifiers(modifiers);
    return actions.find(action => action.id === wanted)
        ?? actions.find(action => action.handler === wanted)
        ?? actions[0];
}
