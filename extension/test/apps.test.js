import {test} from 'node:test';
import assert from 'node:assert/strict';

import {appCandidates, appIdFromResultId, appResultId} from '../lib/apps.js';

const NOW = 1_000_000_000_000;

const APP = {
    id: 'org.gnome.Nautilus.desktop',
    name: 'Files',
    description: 'Browse your files',
    icon: 'org.gnome.Nautilus',
};

test('an application that does not match is dropped', () => {
    assert.deepEqual(appCandidates([APP], 'zzz', {now: NOW}), []);
    assert.deepEqual(appCandidates([APP], '   ', {now: NOW}), []);
});

test('an exact application name match scores base weight plus full match quality', () => {
    const [files] = appCandidates([APP], 'files', {now: NOW});
    assert.equal(files.score, 0.30 + 1.0);
});

test('an application result carries the contract result shape', () => {
    const [files] = appCandidates([APP], 'file', {now: NOW});

    assert.deepEqual(files, {
        id: 'app:org.gnome.Nautilus.desktop',
        provider: 'applications',
        title: 'Files',
        subtitle: 'Browse your files',
        icon: 'org.gnome.Nautilus',
        score: 0.30 + 0.8,
        actions: [
            {id: 'open', label: 'Open', handler: 'open'},
            {id: 'new-window', label: 'New Window', handler: 'open'},
        ],
        preview: {kind: 'application', comment: 'Browse your files'},
    });
});

test('a missing description yields an empty subtitle and comment, not undefined', () => {
    const [app] = appCandidates([{id: 'a.desktop', name: 'Alpha', icon: 'a'}], 'alpha', {now: NOW});
    assert.equal(app.subtitle, '');
    assert.deepEqual(app.preview, {kind: 'application', comment: ''});
    assert.equal(app.icon, 'a');
});

test('matching is on the title, not the description', () => {
    assert.deepEqual(
        appCandidates([{...APP, description: 'open anything you like'}], 'anything', {now: NOW}),
        [],
    );
});

test('results are sorted by score, ties by title', () => {
    const apps = [
        {...APP, id: 'b.desktop', name: 'Files'},
        {...APP, id: 'a.desktop', name: 'Files'},
        {id: 'z.desktop', name: 'Filesystem', description: '', icon: ''},
    ];
    const results = appCandidates(apps, 'files', {now: NOW});

    assert.deepEqual(results.map(r => r.id), [
        'app:a.desktop',
        'app:b.desktop',
        'app:z.desktop',
    ]);
    assert.ok(results[1].score > results[2].score);
});

test('usage raises a score and recency decays with age', () => {
    const used = appCandidates([{...APP, usage: 20}], 'files', {now: NOW})[0];
    const unused = appCandidates([APP], 'files', {now: NOW})[0];
    assert.equal(used.score, unused.score + 0.20);

    const fresh = appCandidates([{...APP, timestamp: NOW}], 'files', {now: NOW})[0];
    const stale = appCandidates(
        [{...APP, timestamp: NOW - 29 * 24 * 60 * 60 * 1000}], 'files', {now: NOW})[0];
    assert.ok(fresh.score > stale.score);
    assert.ok(stale.score >= unused.score);
});

test('candidates are ranked above the same name from another provider', () => {
    const [app] = appCandidates([APP], 'files', {now: NOW});
    assert.ok(app.score > 0.20 + 1.0, 'an exact app match must outrank an exact file match');
});

test('an application without an id or a name is skipped', () => {
    const apps = [APP, {id: '', name: 'Empty'}, {id: 'no-name.desktop', name: ''}, null];
    assert.deepEqual(appCandidates(apps, 'files', {now: NOW}).map(r => r.id),
        ['app:org.gnome.Nautilus.desktop']);
});

test('result ids round-trip through the app: prefix', () => {
    assert.equal(appResultId('a.desktop'), 'app:a.desktop');
    assert.equal(appIdFromResultId('app:a.desktop'), 'a.desktop');
    assert.equal(appIdFromResultId('file:/home/me/a'), null);
    assert.equal(appIdFromResultId(undefined), null);
});
