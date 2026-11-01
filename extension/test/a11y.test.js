// Accessibility and appearance (ticket 14). The decisions are pure, so they are tested here; the
// last few cases are the wiring guards that keep `overlay.js` and `extension.js` using them
// (contract §18).

import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';

import {
    A11Y_SCHEMA,
    ANIMATIONS_KEY,
    CARD_ACCESSIBLE_NAME,
    ENTRY_ACCESSIBLE_NAME,
    HIGH_CONTRAST_CLASS,
    HIGH_CONTRAST_KEY,
    INTERFACE_SCHEMA,
    PREVIEW_ACCESSIBLE_NAME,
    REDUCED_MOTION_KEY,
    REDUCED_MOTION_OFF,
    RESULTS_ACCESSIBLE_NAME,
    ROW_FALLBACK_ACCESSIBLE_NAME,
    accessibleNameFor,
    accessibilityState,
    sectionAccessibleName,
    shouldAnimate,
} from '../lib/a11y.js';

const extensionDir = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (...parts) => readFileSync(join(extensionDir, ...parts), 'utf8');

test('motion is allowed unless a preference says otherwise', () => {
    assert.equal(shouldAnimate(), true, 'an unanswered preference keeps GNOME\'s default');
    assert.equal(shouldAnimate({}), true);
    assert.equal(shouldAnimate({animationsEnabled: true, highContrast: false}), true);
    assert.equal(shouldAnimate({animationsEnabled: false}), false, 'reduced motion suppresses it');
    assert.equal(shouldAnimate({animationsEnabled: true, highContrast: true}), false,
        'so does high contrast');
    assert.equal(shouldAnimate({animationsEnabled: false, highContrast: true}), false);
});

test('the accessibility state folds the GNOME settings into two booleans', () => {
    assert.deepEqual(accessibilityState(), {animationsEnabled: true, highContrast: false},
        'nothing read yet is GNOME\'s default, not a silent preference');
    assert.deepEqual(accessibilityState({enableAnimations: false}),
        {animationsEnabled: false, highContrast: false});
    assert.deepEqual(accessibilityState({reducedMotion: 'reduce'}),
        {animationsEnabled: false, highContrast: false});
    assert.deepEqual(accessibilityState({reducedMotion: REDUCED_MOTION_OFF}),
        {animationsEnabled: true, highContrast: false});
    assert.deepEqual(accessibilityState({enableAnimations: true, reducedMotion: 'no-preference', highContrast: true}),
        {animationsEnabled: true, highContrast: true});
    assert.deepEqual(accessibilityState({enableAnimations: false, highContrast: true}),
        {animationsEnabled: false, highContrast: true});
});

test('a row is named from its title and subtitle', () => {
    assert.equal(accessibleNameFor({title: 'Firefox', subtitle: 'Web Browser'}),
        'Firefox, Web Browser');
    assert.equal(accessibleNameFor({title: 'Firefox', subtitle: ''}), 'Firefox');
    assert.equal(accessibleNameFor({title: 'Firefox'}), 'Firefox');
    assert.equal(accessibleNameFor({subtitle: 'Web Browser'}), 'Web Browser');
});

test('a row with no text falls back to its provider', () => {
    assert.equal(accessibleNameFor({provider: 'applications'}), 'Applications');
    assert.equal(accessibleNameFor({provider: 'files'}), 'Files and Folders');
    assert.equal(accessibleNameFor({provider: 'made-up'}), 'made-up');
    assert.equal(accessibleNameFor({}), ROW_FALLBACK_ACCESSIBLE_NAME);
    assert.equal(accessibleNameFor(), ROW_FALLBACK_ACCESSIBLE_NAME);
    assert.equal(accessibleNameFor(null), ROW_FALLBACK_ACCESSIBLE_NAME);
});

test('a row\'s name is trimmed and never undefined', () => {
    assert.equal(accessibleNameFor({title: '  Firefox  ', subtitle: '  Web Browser '}),
        'Firefox, Web Browser');
    assert.equal(accessibleNameFor({title: '   ', subtitle: '   '}), ROW_FALLBACK_ACCESSIBLE_NAME);
    assert.equal(accessibleNameFor({title: 42}), '42');
});

test('a section header is named from its label, then its provider', () => {
    assert.equal(sectionAccessibleName({provider: 'files', label: 'Files and Folders'}),
        'Files and Folders');
    assert.equal(sectionAccessibleName({provider: 'files'}), 'Files and Folders');
    assert.equal(sectionAccessibleName({provider: 'made-up'}), 'made-up');
    assert.equal(sectionAccessibleName({}), 'Results');
    assert.equal(sectionAccessibleName(), 'Results');
});

test('the module names the schemas and keys the shell reads', () => {
    assert.equal(A11Y_SCHEMA, 'org.gnome.desktop.a11y.interface');
    assert.equal(INTERFACE_SCHEMA, 'org.gnome.desktop.interface');
    assert.equal(ANIMATIONS_KEY, 'enable-animations');
    assert.equal(REDUCED_MOTION_KEY, 'reduced-motion');
    assert.equal(HIGH_CONTRAST_KEY, 'high-contrast');
    assert.equal(REDUCED_MOTION_OFF, 'no-preference');

    // The names a screen reader hears for the parts that carry no text of their own.
    for (const name of [
        CARD_ACCESSIBLE_NAME, ENTRY_ACCESSIBLE_NAME, RESULTS_ACCESSIBLE_NAME, PREVIEW_ACCESSIBLE_NAME,
    ]) {
        assert.equal(typeof name, 'string');
        assert.ok(name.length > 0);
    }
    assert.equal(HIGH_CONTRAST_CLASS, 'lumen-high-contrast');
});

test('the overlay names and roles every row and control it builds', () => {
    const overlay = read('overlay.js');

    assert.match(overlay, /from '\.\/lib\/a11y\.js'/, 'the overlay uses the pure module');
    assert.match(overlay, /import Atk from 'gi:\/\/Atk'/);
    assert.match(overlay, /shouldAnimate\(/, 'the motion decision is the module\'s');
    assert.match(overlay, /accessibleNameFor\(/);
    assert.match(overlay, /sectionAccessibleName\(/);
    assert.match(overlay, /HIGH_CONTRAST_CLASS/);

    // Every part a screen reader has to reach: the card, the entry, the list, each row, each
    // section header, the status line and the preview pane.
    const names = overlay.match(/set_accessible_name\(/g) ?? [];
    assert.ok(names.length >= 6, `only ${names.length} accessible names are set`);
    const roles = overlay.match(/set_accessible_role\(/g) ?? [];
    assert.ok(roles.length >= 5, `only ${roles.length} accessible roles are set`);

    for (const role of ['LIST_ITEM', 'LIST', 'HEADING', 'ENTRY', 'PANEL'])
        assert.match(overlay, new RegExp(`Atk\\.Role\\.${role}\\b`), `no ${role} role`);
});

test('the overlay applies the accessibility state it is handed', () => {
    const overlay = read('overlay.js');
    assert.match(overlay, /setAccessibility\(/);

    // The high-contrast theme is a style class on the backdrop, like the appearance is, so the
    // stylesheet can reach the card and everything inside it.
    assert.match(overlay, /add_style_class_name\(HIGH_CONTRAST_CLASS\)/);
    assert.match(overlay, /remove_style_class_name\(HIGH_CONTRAST_CLASS\)/);

    const css = read('stylesheet.css');
    assert.match(css, /\.lumen-high-contrast[\s,{:]+/, 'the high-contrast theme has no rules');
});

test('the extension observes the accessibility preferences and feeds the overlay', () => {
    const extension = read('extension.js');

    assert.match(extension, /from '\.\/lib\/a11y\.js'/);
    assert.match(extension, /accessibilityState\(/);
    assert.match(extension, /setAccessibility\(/);

    // The keys are read from the schemas the pure module names.
    assert.match(extension, /A11Y_SCHEMA/);
    assert.match(extension, /INTERFACE_SCHEMA/);
    for (const key of ['ANIMATIONS_KEY', 'REDUCED_MOTION_KEY', 'HIGH_CONTRAST_KEY'])
        assert.match(extension, new RegExp(key), `${key} is never read`);

    // A preference changed in the settings app has to reach a running overlay.
    assert.match(extension, /changed::\$\{ANIMATIONS_KEY\}/);
    assert.match(extension, /changed::\$\{HIGH_CONTRAST_KEY\}/);
    assert.match(extension, /changed::\$\{REDUCED_MOTION_KEY\}/);
});

test('Tab moves between sections, through the navigator', () => {
    const extension = read('extension.js');
    assert.match(extension, /sectionStarts\(/, 'the shell tells the navigator where sections start');
    assert.match(extension, /navigator\.nextSection\(\)/);
    assert.match(extension, /navigator\.prevSection\(\)/);
});
