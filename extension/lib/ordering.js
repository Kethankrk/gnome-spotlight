// One ordering, shared by the candidate builders (`apps.js`, `actions.js`) and the view model:
// score descending, then the row's title, then its id, so two halves of the UI cannot disagree
// about what "best first" means. Pure JS: no `gi://` imports, so `node --test` loads it.

/** Locale-independent string order; a missing value sorts before any string. */
export function compareText(a, b) {
    const left = a ?? '';
    const right = b ?? '';
    return left < right ? -1 : left > right ? 1 : 0;
}

/** Title, then id: what breaks a tie between two equally good rows. */
export function compareByTitle(a, b) {
    return compareText(a?.title, b?.title) || compareText(a?.id, b?.id);
}

/** Score descending, then title, then id — the order a list of candidates is presented in. */
export function compareCandidates(a, b) {
    return (b?.score ?? 0) - (a?.score ?? 0) || compareByTitle(a, b);
}
