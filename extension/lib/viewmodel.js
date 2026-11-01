// The daemon ranks every result globally; this flattens that one list into the Top Hit plus the
// fixed sections the overlay renders. `flat` is the same order rows appear on screen, so a row
// index is a `flat` index.

import {PROVIDER_IDS, PROVIDER_LABELS, SECTION_CAPS, SECTION_ORDER} from './protocol.js';

export function buildViewModel(results) {
    const known = (results ?? []).filter(result => PROVIDER_IDS.includes(result?.provider));
    const sorted = dedupe([...known].sort(compareResults));
    const topHit = sorted.length > 0 ? sorted[0] : null;
    const sections = [];
    const flat = topHit ? [topHit] : [];

    for (const provider of SECTION_ORDER) {
        const cap = SECTION_CAPS[provider] ?? 0;
        const items = sorted
            .filter(result => result.provider === provider && result !== topHit)
            .slice(0, cap);
        if (items.length === 0)
            continue;

        sections.push({provider, label: PROVIDER_LABELS[provider], items});
        flat.push(...items);
    }

    return {topHit, sections, flat};
}

/** Score descending, then section order, then title — contract §5. */
export function compareResults(a, b) {
    const byScore = (b?.score ?? 0) - (a?.score ?? 0);
    if (byScore !== 0)
        return byScore;

    const byProvider = providerRank(a?.provider) - providerRank(b?.provider);
    if (byProvider !== 0)
        return byProvider;

    return compareText(a?.title, b?.title) || compareText(a?.id, b?.id);
}

export function providerRank(provider) {
    const rank = PROVIDER_IDS.indexOf(provider);
    return rank === -1 ? PROVIDER_IDS.length : rank;
}

function dedupe(sorted) {
    const seen = new Set();
    return sorted.filter(result => {
        const id = result?.id;
        if (id === undefined || id === null)
            return true;
        if (seen.has(id))
            return false;
        seen.add(id);
        return true;
    });
}

function compareText(a, b) {
    const left = a ?? '';
    const right = b ?? '';
    return left < right ? -1 : left > right ? 1 : 0;
}
