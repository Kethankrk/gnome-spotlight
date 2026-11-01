import {test} from 'node:test';
import assert from 'node:assert/strict';

import {buildPreview, PREVIEW_EMPTY_TEXT} from '../lib/preview.js';

const NOW = 1_700_000_000_000; // 2023-11-14T22:13:20Z — `now` is always injected, never the clock.

const result = (provider, preview, over = {}) => ({
    id: `${provider}:x`,
    provider,
    title: 'Selected Result',
    subtitle: '',
    icon: '',
    score: 1,
    actions: [],
    preview,
    ...over,
});

/** The `rows` as `label: value` pairs, for readable assertions. */
const rowsOf = preview => preview.rows.map(row => `${row.label}: ${row.value}`);

test('an application preview names the result and shows its comment', () => {
    const preview = buildPreview(result('applications', {
        kind: 'application',
        comment: 'Browse your files',
    }));

    assert.equal(preview.title, 'Selected Result');
    assert.deepEqual(rowsOf(preview), [
        'Kind: Application',
        'Comment: Browse your files',
    ]);
});

test('an application with no comment still has something to show', () => {
    const preview = buildPreview(result('applications', {kind: 'application', comment: ''}));
    assert.deepEqual(rowsOf(preview), ['Kind: Application']);
});

test('a file preview shows its path, size and modification time', () => {
    const preview = buildPreview(result('files', {
        kind: 'file',
        path: '/home/me/notes.md',
        size: 1536,
        mtime: NOW - 2 * 60 * 60 * 1000,
        folder: '/home/me',
    }), {now: NOW});

    assert.equal(preview.title, 'Selected Result');
    assert.deepEqual(rowsOf(preview), [
        'Kind: File',
        'Path: /home/me/notes.md',
        'Size: 1.5 kB',
        'Modified: 2 hours ago',
        'Folder: /home/me',
    ]);
});

test('a folder is labelled as a folder', () => {
    const preview = buildPreview(result('files', {
        kind: 'folder',
        path: '/home/me/Documents',
        mtime: NOW - 3 * 24 * 60 * 60 * 1000,
    }), {now: NOW});

    assert.deepEqual(rowsOf(preview), [
        'Kind: Folder',
        'Path: /home/me/Documents',
        'Modified: 3 days ago',
    ]);
});

test('a recent document is rendered like a file, under its own label', () => {
    const preview = buildPreview(result('recent', {
        kind: 'file',
        path: '/home/me/report.odt',
        size: 2_000_000,
    }));

    assert.deepEqual(rowsOf(preview), [
        'Kind: File',
        'Path: /home/me/report.odt',
        'Size: 2.0 MB',
    ]);

    const explicit = buildPreview(result('recent', {
        kind: 'recent',
        path: '/home/me/report.odt',
    }));
    assert.deepEqual(rowsOf(explicit)[0], 'Kind: Recent Document');
});

test('a missing file field leaves no empty row behind', () => {
    const preview = buildPreview(result('files', {kind: 'file', path: '/home/me/notes.md'}));
    assert.deepEqual(rowsOf(preview), ['Kind: File', 'Path: /home/me/notes.md']);
    for (const row of preview.rows)
        assert.equal(typeof row.value, 'string');
});

test('an empty file is still 0 B, not a blank row', () => {
    const preview = buildPreview(result('files', {kind: 'file', path: '/home/me/empty', size: 0}));
    assert.deepEqual(rowsOf(preview), ['Kind: File', 'Path: /home/me/empty', 'Size: 0 B']);
});

test('sizes scale through the units', () => {
    const sizeRow = bytes => buildPreview(result('files', {
        kind: 'file', path: '/x', size: bytes,
    })).rows.find(row => row.label === 'Size').value;

    assert.equal(sizeRow(512), '512 B');
    assert.equal(sizeRow(999), '999 B');
    assert.equal(sizeRow(1000), '1.0 kB');
    assert.equal(sizeRow(1536), '1.5 kB');
    assert.equal(sizeRow(2_000_000), '2.0 MB');
    assert.equal(sizeRow(3_500_000_000), '3.5 GB');
    assert.equal(sizeRow(1_000_000_000_000), '1.0 TB');
});

test('a size the daemon sends as a bigint or a string is still formatted', () => {
    const sizeRow = size => buildPreview(result('files', {
        kind: 'file', path: '/x', size,
    })).rows.find(row => row.label === 'Size')?.value ?? null;

    assert.equal(sizeRow(1536n), '1.5 kB');
    assert.equal(sizeRow('2048'), '2.0 kB');
    assert.equal(sizeRow(-1), null);
    assert.equal(sizeRow('not a size'), null);
});

test('timestamps are read as milliseconds, and as seconds when they are small', () => {
    const modified = value => buildPreview(
        result('files', {kind: 'file', path: '/x', mtime: value}),
        {now: NOW},
    ).rows.find(row => row.label === 'Modified')?.value ?? null;

    const twoHoursAgo = NOW - 2 * 60 * 60 * 1000;
    assert.equal(modified(twoHoursAgo), '2 hours ago');
    assert.equal(modified(twoHoursAgo / 1000), '2 hours ago');
    assert.equal(modified(0), null);
    assert.equal(modified(undefined), null);
});

test('ages are relative while they are recent and a date once they are old', () => {
    const modified = value => buildPreview(
        result('files', {kind: 'file', path: '/x', mtime: value}),
        {now: NOW},
    ).rows.find(row => row.label === 'Modified').value;

    assert.equal(modified(NOW - 5_000), 'just now');
    assert.equal(modified(NOW - 60_000), '1 minute ago');
    assert.equal(modified(NOW - 5 * 60_000), '5 minutes ago');
    assert.equal(modified(NOW - 60 * 60_000), '1 hour ago');
    assert.equal(modified(NOW - 30 * 24 * 60 * 60_000), '30 days ago');
    assert.equal(modified(NOW - 31 * 24 * 60 * 60_000), '2023-10-14');
});

test('a clipboard preview shows the text and when it was copied', () => {
    const preview = buildPreview(result('clipboard', {
        kind: 'clipboard',
        text: 'curl https://example.com',
        timestamp: NOW - 90_000,
    }), {now: NOW});

    assert.deepEqual(rowsOf(preview), [
        'Kind: Clipboard',
        'Text: curl https://example.com',
        'Copied: 1 minute ago',
    ]);
});

test('a long clipboard entry is trimmed and flattened to one run of text', () => {
    const preview = buildPreview(result('clipboard', {
        kind: 'clipboard',
        text: `line one\n\n   line two ${'x'.repeat(400)}`,
    }), {now: NOW});

    const text = preview.rows.find(row => row.label === 'Text').value;
    assert.ok(text.startsWith('line one line two '), text.slice(0, 40));
    assert.ok(!/\s\s/.test(text), 'runs of whitespace are collapsed');
    assert.equal(text.length, 240);
    assert.ok(text.endsWith('…'));
});

test('a clipboard entry with no text keeps its own row set', () => {
    const preview = buildPreview(result('clipboard', {kind: 'clipboard', text: ''}));
    assert.deepEqual(rowsOf(preview), ['Kind: Clipboard']);
});

test('a calculation shows the expression, the value and the unit', () => {
    const preview = buildPreview(result('calculator', {
        kind: 'calculator',
        expression: '10 km in miles',
        value: '6.21371',
        unit: 'miles',
    }));

    assert.deepEqual(rowsOf(preview), [
        'Kind: Calculation',
        'Expression: 10 km in miles',
        'Value: 6.21371',
        'Unit: miles',
    ]);
});

test('a plain calculation has no unit row', () => {
    const preview = buildPreview(result('calculator', {
        kind: 'calculator', expression: '2 + 2', value: '4', unit: '',
    }));
    assert.deepEqual(rowsOf(preview), [
        'Kind: Calculation',
        'Expression: 2 + 2',
        'Value: 4',
    ]);
});

test('a text preview shows its detail line', () => {
    const preview = buildPreview(result('actions', {kind: 'text', text: 'Lock the screen'}));
    assert.deepEqual(rowsOf(preview), ['Details: Lock the screen']);
});

test('results with nothing to show degrade to an empty row set', () => {
    const empty = [
        null,
        undefined,
        {title: 'No preview at all'},
        result('websearch', {}),
        result('websearch', {kind: 'websearch'}),
        result('actions', {}),
        result('files', {}),
        result('calculator', {}),
    ];

    for (const input of empty) {
        const preview = buildPreview(input);
        assert.deepEqual(preview.rows, [], `expected no rows for ${JSON.stringify(input)}`);
        assert.equal(typeof preview.title, 'string');
    }

    assert.deepEqual(buildPreview(null), {title: '', rows: []});
    assert.deepEqual(buildPreview(result('websearch', {})).title, 'Selected Result');
    assert.equal(typeof PREVIEW_EMPTY_TEXT, 'string');
    assert.ok(PREVIEW_EMPTY_TEXT.length > 0);
});

test('a missing title is an empty string rather than undefined', () => {
    assert.equal(buildPreview(result('files', {kind: 'file', path: '/x'}, {title: undefined})).title, '');
    assert.equal(buildPreview(result('files', {kind: 'file', path: '/x'}, {title: 42})).title, '');
});

test('a payload that carries only a kind still says what the result is', () => {
    assert.deepEqual(rowsOf(buildPreview(result('files', {kind: 'file'}))), ['Kind: File']);
    assert.deepEqual(
        rowsOf(buildPreview(result('applications', {kind: 'application'}))),
        ['Kind: Application'],
    );
    assert.deepEqual(
        rowsOf(buildPreview(result('calculator', {kind: 'calculator'}))),
        ['Kind: Calculation'],
    );
});

test('a payload without a kind is read through its provider', () => {
    assert.deepEqual(
        rowsOf(buildPreview(result('files', {path: '/home/me/notes.md'}))),
        ['Kind: File', 'Path: /home/me/notes.md'],
    );
    assert.deepEqual(
        rowsOf(buildPreview(
            result('clipboard', {text: 'copied', timestamp: NOW}),
            {now: NOW + 60_000},
        )),
        ['Kind: Clipboard', 'Text: copied', 'Copied: 1 minute ago'],
    );
});

test('an unknown kind falls back to whatever detail the payload carries', () => {
    assert.deepEqual(
        rowsOf(buildPreview(result('files', {kind: 'image', path: '/home/me/cat.png'}))),
        ['Path: /home/me/cat.png'],
    );
    assert.deepEqual(
        rowsOf(buildPreview(result('files', {kind: 'image', text: 'a picture'}))),
        ['Details: a picture'],
    );
    assert.deepEqual(buildPreview(result('files', {kind: 'image'})).rows, []);
});

test('every row has a non-empty label and a string value', () => {
    const samples = [
        result('applications', {kind: 'application', comment: 'Browse your files'}),
        result('files', {kind: 'file', path: '/x', size: 10, mtime: NOW, folder: '/y'}),
        result('clipboard', {kind: 'clipboard', text: 'hi', timestamp: NOW}),
        result('calculator', {kind: 'calculator', expression: '1+1', value: '2', unit: ''}),
        result('actions', {kind: 'text', text: 'Lock'}),
    ];

    for (const sample of samples) {
        const preview = buildPreview(sample, {now: NOW});
        assert.ok(preview.rows.length > 0, `${sample.provider} should have rows`);
        for (const row of preview.rows) {
            assert.equal(typeof row.label, 'string');
            assert.notEqual(row.label, '');
            assert.equal(typeof row.value, 'string');
            assert.notEqual(row.value, '');
        }
    }
});

test('the payload is not mutated, so the same result can be previewed twice', () => {
    const payload = {kind: 'file', path: '/x', size: 1000, mtime: NOW};
    const sample = result('files', payload);

    const first = buildPreview(sample, {now: NOW});
    const second = buildPreview(sample, {now: NOW});

    assert.deepEqual(first, second);
    assert.deepEqual(payload, {kind: 'file', path: '/x', size: 1000, mtime: NOW});
});
