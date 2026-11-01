// The applications provider. The shell hands in its `Shell.AppSystem` entries as plain objects;
// this turns the matches into contract §3 results for `Query.options.injected`. The daemon then
// re-scores and ranks them alongside its own providers.

import {ID_PREFIXES} from './protocol.js';
import {matchQuality, score} from './scoring.js';

export const APPLICATION_PROVIDER = 'applications';

const OPEN = {id: 'open', label: 'Open', handler: 'open'};
const NEW_WINDOW = {id: 'new-window', label: 'New Window', handler: 'open'};

export function appResultId(appId) {
    return `${ID_PREFIXES[APPLICATION_PROVIDER]}${appId}`;
}

export function appIdFromResultId(resultId) {
    const prefix = ID_PREFIXES[APPLICATION_PROVIDER];
    return typeof resultId === 'string' && resultId.startsWith(prefix)
        ? resultId.slice(prefix.length)
        : null;
}

/**
 * Matching applications as result rows, best first. `apps` entries are
 * `{id, name, description, icon, usage?, timestamp?}`; a candidate that does not match the query
 * on its name is dropped, and an empty query matches nothing.
 */
export function appCandidates(apps, query, {now = Date.now()} = {}) {
    const needle = (query ?? '').trim();
    if (needle === '')
        return [];

    const results = [];
    for (const app of apps ?? []) {
        if (!app?.id || !app.name || matchQuality(app.name, needle) === 0)
            continue;

        const comment = app.description ?? '';
        results.push({
            id: appResultId(app.id),
            provider: APPLICATION_PROVIDER,
            title: app.name,
            subtitle: comment,
            icon: app.icon ?? '',
            score: score(
                {
                    provider: APPLICATION_PROVIDER,
                    title: app.name,
                    usage: app.usage,
                    timestamp: app.timestamp,
                },
                needle,
                {now},
            ),
            actions: [{...OPEN}, {...NEW_WINDOW}],
            preview: {kind: 'application', comment},
        });
    }

    return results.sort(compareCandidates);
}

function compareCandidates(a, b) {
    return b.score - a.score
        || compareText(a.title, b.title)
        || compareText(a.id, b.id);
}

function compareText(a, b) {
    return a < b ? -1 : a > b ? 1 : 0;
}
