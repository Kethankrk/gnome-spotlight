// The scorer, mirroring the daemon's Rust implementation (contract §5). Pure JS.

export const BASE_WEIGHT = {
    applications: 0.30,
    calculator: 0.40,
    actions: 0.25,
    settings: 0.25,
    files: 0.20,
    recent: 0.18,
    clipboard: 0.15,
    websearch: 0.05,
};

export const RECENCY_HORIZON_MS = 30 * 24 * 60 * 60 * 1000;
export const USAGE_CAP = 20;

export function matchQuality(title, query) {
    const text = (title ?? '').toLowerCase();
    const needle = (query ?? '').toLowerCase().trim();
    if (needle === '')
        return 0.0;

    if (text === needle)
        return 1.0;
    if (text.startsWith(needle))
        return 0.8;
    if (text.split(/[\s\-_/.]/).some(word => word.startsWith(needle)))
        return 0.6;
    if (text.includes(needle))
        return 0.4;
    if (isSubsequence(text, needle))
        return 0.2;
    return 0.0;
}

function isSubsequence(text, needle) {
    let i = 0;
    for (const ch of text) {
        if (ch === needle[i])
            i += 1;
        if (i === needle.length)
            return true;
    }
    return needle.length === 0;
}

export function recencyBonus(timestampMs, nowMs) {
    if (timestampMs == null)
        return 0.0;
    const age = Math.max(0, nowMs - timestampMs);
    return clamp(0.10 * (1 - age / RECENCY_HORIZON_MS), 0.0, 0.10);
}

export function usageBonus(count) {
    if (!count)
        return 0.0;
    return 0.20 * Math.min(count, USAGE_CAP) / USAGE_CAP;
}

// The web-search row is the exception: its title is spelled out of the query ("Search the web for
// …"), so matching it against the query would always score it as a hit and float the last-resort
// row above real results. It is scored by its base weight alone (ticket 08, contract §15).
export function score(candidate, query, context = {}) {
    const provider = candidate.provider;
    const base = BASE_WEIGHT[provider] ?? 0.0;
    const now = context.now ?? Date.now();
    const quality = provider === 'websearch' ? 0.0 : matchQuality(candidate.title, query);
    return base
        + quality
        + recencyBonus(candidate.timestamp, now)
        + usageBonus(candidate.usage);
}

function clamp(value, low, high) {
    return Math.min(high, Math.max(low, value));
}
