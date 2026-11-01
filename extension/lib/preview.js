// The preview pane model (contract §6). The daemon ships a provider-specific `preview` payload
// with every result; this turns it into the title and the `{label, value}` rows the overlay
// renders under the result list. Pure JS — no shell imports — so `node --test` drives it.
//
// Rules, so that no result ever produces a broken pane:
// - a missing or empty payload degrades to `{title, rows: []}` (the overlay shows its muted
//   empty state instead of an empty box);
// - a payload that names a `kind` is rendered by kind;
// - a payload without a `kind` is read through the result's provider, which is a 1:1 mapping;
// - a payload that carries only a `kind` still shows what the result is;
// - rows whose detail is absent are dropped rather than rendered blank;
// - timestamps are taken as milliseconds — `t` on the wire — and as seconds when they are small
//   enough to be one (a `t` dated before 1973), so a daemon that sends either degrades sanely;
// - ages are relative while recent and an absolute UTC date once older than 30 days, which keeps
//   the pane's output independent of the machine's locale and time zone.

/** Shown by the overlay when a result has nothing to show (story 53). */
export const PREVIEW_EMPTY_TEXT = 'No details for this result';

const KIND_LABELS = {
    application: 'Application',
    file: 'File',
    folder: 'Folder',
    recent: 'Recent Document',
    clipboard: 'Clipboard',
    calculator: 'Calculation',
};

// Contract §6 gives every provider payload a kind; these defaults cover a daemon that omits it.
const PROVIDER_KINDS = {
    applications: 'application',
    files: 'file',
    recent: 'file',
    clipboard: 'clipboard',
    calculator: 'calculator',
};

const MAX_TEXT = 240;
const SIZE_UNITS = ['kB', 'MB', 'GB', 'TB', 'PB'];
// Anything below this is too small to be a millisecond epoch (1e11 ms is 1973), so it is seconds.
const SECONDS_CUTOFF = 1e11;
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const RELATIVE_LIMIT = 30 * DAY;

/**
 * The preview pane's contents for one result: `{title, rows: [{label, value}]}`.
 * `now` is injected so the relative ages are testable; it defaults to the current time.
 */
export function buildPreview(result, {now = Date.now()} = {}) {
    const title = typeof result?.title === 'string' ? result.title : '';
    const payload = result?.preview;

    if (!isRecord(payload) || Object.keys(payload).length === 0)
        return {title, rows: []};

    const rows = rowsFor(kindOf(payload, result), payload, now).filter(Boolean);
    return {title, rows};
}

function rowsFor(kind, payload, now) {
    switch (kind) {
    case 'text':
        return detailRows(payload);
    case 'application':
        return [
            kindRow(kind),
            row('Comment', textOf(payload.comment)),
        ];
    case 'file':
    case 'folder':
    case 'recent':
        return [
            kindRow(kind),
            row('Path', textOf(payload.path)),
            row('Size', formatSize(payload.size)),
            row('Modified', formatAge(payload.mtime, now)),
            row('Folder', textOf(payload.folder)),
        ];
    case 'clipboard':
        return [
            kindRow(kind),
            row('Text', truncate(collapse(textOf(payload.text)))),
            row('Copied', formatAge(payload.timestamp, now)),
        ];
    case 'calculator':
        return [
            kindRow(kind),
            row('Expression', textOf(payload.expression)),
            row('Value', textOf(payload.value)),
            row('Unit', textOf(payload.unit)),
        ];
    default:
        // An unknown kind (or a web-search row, whose payload is empty) still shows whatever
        // usable detail it came with.
        return detailRows(payload);
    }
}

function detailRows(payload) {
    return [
        row('Details', truncate(collapse(textOf(payload.text)))),
        row('Path', textOf(payload.path)),
    ];
}

function kindOf(payload, result) {
    const kind = textOf(payload.kind).trim();
    if (kind !== '')
        return kind;

    return PROVIDER_KINDS[textOf(result?.provider)] ?? '';
}

function kindRow(kind) {
    return {label: 'Kind', value: KIND_LABELS[kind]};
}

/** A row, or null when there is nothing to put in it — never a blank value. */
function row(label, value) {
    return value === '' ? null : {label, value};
}

/** Human readable bytes: 1536 → "1.5 kB". */
function formatSize(value) {
    const bytes = number(value);
    if (bytes === null || bytes < 0)
        return '';

    if (bytes < 1000)
        return `${Math.round(bytes)} B`;

    let size = bytes;
    let unit = 0;
    while (size >= 1000 && unit < SIZE_UNITS.length) {
        size /= 1000;
        unit++;
    }

    return `${size.toFixed(1)} ${SIZE_UNITS[unit - 1]}`;
}

/** "just now" / "5 minutes ago" / "30 days ago" / "2023-10-14" (UTC). */
function formatAge(value, now) {
    const ms = milliseconds(value);
    if (ms === null)
        return '';

    const delta = now - ms;
    if (delta < MINUTE)
        return 'just now';
    if (delta < HOUR)
        return plural(delta / MINUTE, 'minute');
    if (delta < DAY)
        return plural(delta / HOUR, 'hour');
    if (delta <= RELATIVE_LIMIT)
        return plural(delta / DAY, 'day');

    return new Date(ms).toISOString().slice(0, 10);
}

function plural(count, noun) {
    const whole = Math.max(1, Math.floor(count));
    return `${whole} ${noun}${whole === 1 ? '' : 's'} ago`;
}

function milliseconds(value) {
    const n = number(value);
    if (n === null || n <= 0)
        return null;

    return n < SECONDS_CUTOFF ? n * 1000 : n;
}

function number(value) {
    if (typeof value === 'bigint')
        return Number(value);
    if (typeof value === 'number')
        return Number.isFinite(value) ? value : null;
    if (typeof value === 'string' && value.trim() !== '' && Number.isFinite(Number(value)))
        return Number(value);

    return null;
}

function textOf(value) {
    if (typeof value === 'string')
        return value;
    if (typeof value === 'number' || typeof value === 'bigint' || typeof value === 'boolean')
        return String(value);

    return '';
}

/** Collapse runs of whitespace: a multi-line clipboard entry stays a single preview line. */
function collapse(text) {
    return text.replace(/\s+/g, ' ').trim();
}

function truncate(text) {
    return text.length > MAX_TEXT ? `${text.slice(0, MAX_TEXT - 1)}…` : text;
}

function isRecord(value) {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}
