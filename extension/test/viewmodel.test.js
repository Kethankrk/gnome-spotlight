import {test} from 'node:test';
import assert from 'node:assert/strict';

import {buildViewModel} from '../lib/viewmodel.js';
import {PROVIDER_LABELS, SECTION_CAPS, SECTION_ORDER} from '../lib/protocol.js';

const result = (provider, title, score, id = `${provider}:${title}`) => ({
    id,
    provider,
    title,
    subtitle: '',
    icon: '',
    score,
    actions: [],
    preview: {},
});

test('an empty result list has no top hit and no sections', () => {
    assert.deepEqual(buildViewModel([]), {topHit: null, sections: [], flat: []});
    assert.deepEqual(buildViewModel(null), {topHit: null, sections: [], flat: []});
});

test('the highest scoring result is the top hit and is not repeated in its section', () => {
    const view = buildViewModel([
        result('files', 'notes.md', 0.7),
        result('applications', 'Notes', 1.3),
        result('applications', 'Nautilus', 0.5),
    ]);

    assert.equal(view.topHit.title, 'Notes');
    const apps = view.sections.find(section => section.provider === 'applications');
    assert.deepEqual(apps.items.map(item => item.title), ['Nautilus']);
});

test('sections are ordered by SECTION_ORDER and labelled from PROVIDER_LABELS', () => {
    const view = buildViewModel([
        result('websearch', 'Search the web', 0.05),
        result('applications', 'Terminal', 0.9),
        result('files', 'terminal.sh', 0.6),
        result('recent', 'terminal-notes.txt', 0.4),
    ]);

    assert.equal(view.topHit.title, 'Terminal');
    assert.deepEqual(
        view.sections.map(section => section.provider),
        ['files', 'recent', 'websearch'],
    );
    for (const section of view.sections)
        assert.equal(section.label, PROVIDER_LABELS[section.provider]);

    const present = new Set(view.sections.map(section => section.provider));
    assert.deepEqual(
        view.sections.map(section => section.provider),
        SECTION_ORDER.filter(provider => present.has(provider)),
    );
});

test('empty sections are omitted', () => {
    const view = buildViewModel([result('calculator', '4', 1.4)]);
    assert.equal(view.topHit.title, '4');
    assert.deepEqual(view.sections, []);
});

test('each section is capped, and the cap is applied after the top hit is removed', () => {
    const apps = Array.from({length: SECTION_CAPS.applications + 3}, (_, i) =>
        result('applications', `App ${i}`, 1.0 - i / 100));
    const view = buildViewModel(apps);

    const section = view.sections.find(s => s.provider === 'applications');
    assert.equal(section.items.length, SECTION_CAPS.applications);
    assert.deepEqual(
        section.items.map(item => item.title),
        apps.slice(1, 1 + SECTION_CAPS.applications).map(item => item.title),
    );
});

test('sections of one item are still capped at the contract cap', () => {
    const calculators = Array.from({length: 5}, (_, i) =>
        result('calculator', `calc ${i}`, 0.5 - i / 100));
    const view = buildViewModel([
        result('applications', 'Terminal', 1.3),
        ...calculators,
    ]);
    assert.equal(view.sections.find(s => s.provider === 'calculator').items.length,
        SECTION_CAPS.calculator);
});

test('a caller may raise a section cap, which is how paging shows more files', () => {
    const files = Array.from({length: SECTION_CAPS.files + 12}, (_, i) =>
        result('files', `report-${i}.md`, 1.0 - i / 100, `file:/home/me/report-${i}.md`));

    const byDefault = buildViewModel(files);
    assert.equal(
        byDefault.sections.find(s => s.provider === 'files').items.length,
        SECTION_CAPS.files,
        'the contract cap is the default',
    );

    const raised = buildViewModel(files, {caps: {files: files.length}});
    assert.equal(
        raised.sections.find(s => s.provider === 'files').items.length,
        files.length - 1,
        'the top hit is one of the files, and the rest of the page is shown',
    );

    // Only the section that was named changes.
    const other = buildViewModel(files, {caps: {clipboard: 1}});
    assert.equal(
        other.sections.find(s => s.provider === 'files').items.length,
        SECTION_CAPS.files,
    );
});

test('the flat list is the top hit followed by the sections, so row index == flat index', () => {
    const view = buildViewModel([
        result('applications', 'Notes', 1.3),
        result('files', 'notes.md', 0.7),
        result('websearch', 'Search the web', 0.05),
    ]);

    assert.deepEqual(view.flat.map(item => item.title), ['Notes', 'notes.md', 'Search the web']);
    assert.equal(view.flat[0], view.topHit);
    assert.equal(view.flat.length,
        1 + view.sections.reduce((total, section) => total + section.items.length, 0));
});

test('ties break by section order, then by title', () => {
    const view = buildViewModel([
        result('files', 'beta', 0.6),
        result('applications', 'alpha', 0.6),
        result('files', 'alpha', 0.6),
        result('applications', 'beta', 0.6),
    ]);

    assert.equal(view.topHit.provider, 'applications');
    assert.equal(view.topHit.title, 'alpha');
    assert.deepEqual(
        view.flat.map(item => `${item.provider}/${item.title}`),
        ['applications/alpha', 'applications/beta', 'files/alpha', 'files/beta'],
    );
});

test('a result id appears at most once, even if the daemon sends it twice', () => {
    const view = buildViewModel([
        result('files', 'notes.md', 0.7, 'file:/home/me/notes.md'),
        result('files', 'notes.md', 0.5, 'file:/home/me/notes.md'),
    ]);

    assert.equal(view.topHit.title, 'notes.md');
    const total = view.flat.length;
    assert.equal(total, 1);
});

test('a provider outside SECTION_ORDER contributes no section', () => {
    const view = buildViewModel([
        result('nonsense', 'what', 9.0),
        result('applications', 'Terminal', 0.9),
    ]);

    assert.equal(view.topHit.title, 'Terminal');
    assert.deepEqual(view.sections, []);
});
