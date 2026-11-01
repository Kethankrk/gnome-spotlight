import {test} from 'node:test';
import assert from 'node:assert/strict';

import {
    MODIFIER_ORDER,
    acceleratorLabel,
    bindingLabel,
    collides,
    flattenBindings,
    normalizeAccelerator,
    parseAccelerator,
    prettifyKeyName,
    sameAccelerator,
} from '../lib/shortcut.js';

// The rules below mirror GTK 4.20's accelerator parsing, verified on the build host:
//   * modifier names are case-insensitive and `Ctrl`/`Primary` mean `Control`;
//     `Mod1`-`Mod5`, `Win` and `Cmd` are *not* modifiers GTK accepts,
//   * `gtk_accelerator_name()` prints modifiers in the order Shift, Control, Alt, Super,
//     Hyper, Meta, whatever order they were written in,
//   * keyval lookup is case-sensitive: `space` parses and `SPACE` does not, `Tab` parses and
//     `tab` does not, while single letters are spelled lowercase (`t`, not `T`).
// The module keeps the key as spelled (so a value that GTK accepts stays accepted) but compares
// keys case-insensitively, so two spellings of one key are reported as a collision.

test('a plain accelerator is split into its modifiers and its key', () => {
    assert.deepEqual(parseAccelerator('<Super>space'), {
        modifiers: ['Super'],
        key: 'space',
        accelerator: '<Super>space',
    });

    assert.deepEqual(parseAccelerator('<Control><Alt>t'), {
        modifiers: ['Control', 'Alt'],
        key: 't',
        accelerator: '<Control><Alt>t',
    });
});

test('a bare key is an accelerator with no modifiers', () => {
    assert.deepEqual(parseAccelerator('Print'), {modifiers: [], key: 'Print', accelerator: 'Print'});
    assert.deepEqual(parseAccelerator('F12'), {modifiers: [], key: 'F12', accelerator: 'F12'});
});

test('the disabled spellings are not an accelerator', () => {
    for (const disabled of ['', '   ', 'Disabled', 'disabled', 'DISABLED', 'none', 'NONE'])
        assert.equal(parseAccelerator(disabled), null, `${JSON.stringify(disabled)} should be empty`);
});

test('anything that is not an accelerator string parses to null', () => {
    for (const junk of [null, undefined, 42, 0, true, {}, [], ['<Super>space'], () => {}])
        assert.equal(parseAccelerator(junk), null);
});

test('a modifier with no key is not an accelerator', () => {
    for (const modifiersOnly of ['<Super>', '<Control><Alt>', '<Shift>', '<Super><Super>'])
        assert.equal(parseAccelerator(modifiersOnly), null, `${modifiersOnly} has no key`);
});

test('unknown modifiers and malformed accelerators are rejected', () => {
    // GTK rejects these spellings, so a binding that contains one is not a binding at all.
    for (const rejected of ['<Mod4>space', '<Mod1>t', '<Win>space', '<Cmd>t', '<Bogus>x',
        '<Superspace', '<>space', '<>', '<Super>x<Shift>'])
        assert.equal(parseAccelerator(rejected), null, `${rejected} should not parse`);
});

test('modifier aliases are normalised the way GTK prints them', () => {
    assert.equal(normalizeAccelerator('<Ctrl>space'), '<Control>space');
    assert.equal(normalizeAccelerator('<control>space'), '<Control>space');
    assert.equal(normalizeAccelerator('<Primary>space'), '<Control>space');
    assert.equal(normalizeAccelerator('<primary><alt>t'), '<Control><Alt>t');
    assert.equal(normalizeAccelerator('<shift><super>space'), '<Shift><Super>space');
    assert.equal(normalizeAccelerator('<Hyper><Meta>t'), '<Hyper><Meta>t');
});

test('modifiers are re-ordered and de-duplicated into GTK canonical order', () => {
    assert.deepEqual(MODIFIER_ORDER, ['Shift', 'Control', 'Alt', 'Super', 'Hyper', 'Meta']);

    // GTK prints Shift, Control, Alt, Super — whatever order they were typed in.
    assert.equal(normalizeAccelerator('<Super><Shift>space'), '<Shift><Super>space');
    assert.equal(
        normalizeAccelerator('<Super><Alt><Control><Shift>t'),
        '<Shift><Control><Alt><Super>t',
    );
    assert.equal(normalizeAccelerator('<Super><Super>space'), '<Super>space');
    assert.equal(normalizeAccelerator('<Ctrl><Primary>t'), '<Control>t');
});

test('single-letter keys are lowercased the way GTK spells them', () => {
    // GTK's keyval name for the T key is `t`; `<Super>T` does not parse at all, so lowering it
    // turns an unusable binding into a usable one.
    assert.equal(normalizeAccelerator('<Super>T'), '<Super>t');
    assert.equal(normalizeAccelerator('<Control><Alt>A'), '<Control><Alt>a');
    // Digits and named keys keep their spelling: `F12`, `Print`, `Tab` and `Page_Up` are GTK's.
    assert.equal(normalizeAccelerator('<Control>1'), '<Control>1');
    assert.equal(normalizeAccelerator('<Super>F12'), '<Super>F12');
    assert.equal(normalizeAccelerator('<Alt>Print'), '<Alt>Print');
    assert.equal(normalizeAccelerator('<Super>Tab'), '<Super>Tab');
    assert.equal(normalizeAccelerator('<Super>Page_Up'), '<Super>Page_Up');
});

test('surrounding and inner whitespace is tolerated', () => {
    assert.equal(normalizeAccelerator('  <Super>space  '), '<Super>space');
    assert.equal(normalizeAccelerator('<Super> space'), '<Super>space');
    assert.equal(normalizeAccelerator(' Print '), 'Print');
});

test('the human spelling from the label is accepted too', () => {
    // A hand-edited value (or one pasted out of the preferences window) may use `+` separators.
    assert.equal(normalizeAccelerator('Super+space'), '<Super>space');
    assert.equal(normalizeAccelerator('Ctrl+Alt+t'), '<Control><Alt>t');
    assert.equal(normalizeAccelerator('shift+super+space'), '<Shift><Super>space');
    // The modifier order is canonicalised here as well.
    assert.equal(normalizeAccelerator('super+shift+space'), '<Shift><Super>space');
    // All modifiers and no key is still no accelerator.
    assert.equal(normalizeAccelerator('super'), null);
    assert.equal(normalizeAccelerator('ctrl+alt'), null);
});

test('normalizeAccelerator returns null exactly when parseAccelerator does', () => {
    const cases = ['<Super>space', 'Print', 'Disabled', '<Super>', '<Mod4>d', 'Ctrl+Alt+t', ''];
    for (const value of cases) {
        const parsed = parseAccelerator(value);
        assert.equal(normalizeAccelerator(value), parsed ? parsed.accelerator : null);
    }
});

test('the label reads like the GTK label', () => {
    assert.equal(acceleratorLabel('<Super>space'), 'Super+Space');
    assert.equal(acceleratorLabel('<Shift><Super>space'), 'Shift+Super+Space');
    assert.equal(acceleratorLabel('<Control><Alt>t'), 'Ctrl+Alt+T');
    assert.equal(acceleratorLabel('<Primary><Alt>Delete'), 'Ctrl+Alt+Delete');
    assert.equal(acceleratorLabel('<Control>1'), 'Ctrl+1');
    assert.equal(acceleratorLabel('F12'), 'F12');
    assert.equal(acceleratorLabel('<Super>Page_Up'), 'Super+Page Up');
    assert.equal(acceleratorLabel('<Super>ISO_Left_Tab'), 'Super+ISO_Left_Tab');
});

test('the label of nothing is null', () => {
    for (const nothing of ['', 'Disabled', '<Super>', null, 42])
        assert.equal(acceleratorLabel(nothing), null);
});

test('two spellings of the same accelerator are equal', () => {
    assert.equal(sameAccelerator('<Super>space', '<Super>space'), true);
    assert.equal(sameAccelerator('<super><shift>space', '<Shift><Super>space'), true);
    assert.equal(sameAccelerator('<Ctrl>t', '<Primary>t'), true);
    assert.equal(sameAccelerator('Super+space', '<Super>space'), true);
    // GTK's keyval lookup is case-sensitive, but two spellings of one key are one collision.
    assert.equal(sameAccelerator('<Super>SPACE', '<Super>space'), true);

    assert.equal(sameAccelerator('<Super>space', '<Shift><Super>space'), false);
    assert.equal(sameAccelerator('<Super>space', '<Super>s'), false);
    assert.equal(sameAccelerator('<Super>space', 'Print'), false);
});

test('an unparseable accelerator is never equal to anything', () => {
    for (const junk of ['', 'Disabled', '<Super>', '<Mod4>space', null, undefined, 42]) {
        assert.equal(sameAccelerator(junk, '<Super>space'), false, `${junk} == <Super>space`);
        assert.equal(sameAccelerator('<Super>space', junk), false, `<Super>space == ${junk}`);
        assert.equal(sameAccelerator(junk, junk), false, `${junk} == itself`);
    }
});

test('a binding that is already taken is reported', () => {
    assert.equal(collides('<Super>space', {existing: ['<Super>space']}), '<Super>space');
    // Normalisation decides: the same binding is caught through its aliases and its order.
    assert.equal(
        collides('<shift><super>SPACE', {existing: ['<Shift><Super>space']}),
        '<Shift><Super>space',
    );
    assert.equal(
        collides('<Ctrl>space', {existing: ['Print', '<Primary>space', '<Super>space']}),
        '<Primary>space',
    );
});

test('a binding object is matched by its accelerator and handed back whole', () => {
    const taken = {accelerator: '<Super>d', label: 'Show desktop'};
    assert.equal(collides('<Super>d', {existing: [taken]}), taken);
    assert.deepEqual(collides('<Super>d', {existing: [taken]}), taken);
});

test('a free accelerator collides with nothing', () => {
    assert.equal(collides('<Super>space', {existing: []}), null);
    assert.equal(collides('<Super>space'), null);
    assert.equal(collides('<Super>space', {existing: ['<Super>d', 'Print']}), null);
    assert.equal(collides('<Super>space', {existing: ['<Shift><Super>space']}), null);
});

test('unusable entries are skipped instead of colliding', () => {
    assert.equal(collides('<Super>space', {existing: ['Disabled', '', '<Super>', '<Mod4>space']}), null);
    // An unusable candidate cannot collide with anything either.
    assert.equal(collides('Disabled', {existing: ['<Super>space']}), null);
    assert.equal(collides('<Super>', {existing: ['<Super>space']}), null);
});

test('collides never mutates or needs the existing list', () => {
    const existing = ['<Super>space', {accelerator: '<Super>d', label: 'Show desktop'}];
    const copy = JSON.stringify(existing);
    collides('<Super>d', {existing});
    assert.equal(JSON.stringify(existing), copy);
    assert.equal(collides('<Super>d', {existing: null}), null);
    assert.equal(collides('<Super>d', {existing: 'nonsense'}), null);
});

test('the first match wins', () => {
    const first = {accelerator: '<Super>space', label: 'Switch input source'};
    const second = {accelerator: '<Super>space', label: 'Something else'};
    assert.equal(collides('<Super>space', {existing: [first, second]}), first);
});

test('taken bindings are flattened out of their settings groups', () => {
    const flattened = flattenBindings([
        {label: 'Show desktop', accelerators: ['<Super>d']},
        {label: 'Terminal', accelerators: ['<Control><Alt>t', 'Disabled']},
        {label: 'Screenshots', accelerators: ['Print', '<Shift>Print']},
    ]);

    assert.deepEqual(flattened, [
        {accelerator: '<Super>d', label: 'Show desktop'},
        {accelerator: '<Control><Alt>t', label: 'Terminal'},
        {accelerator: 'Print', label: 'Screenshots'},
        {accelerator: '<Shift>Print', label: 'Screenshots'},
    ]);
});

test('flattenBindings drops what it cannot use and keeps one entry per accelerator', () => {
    assert.deepEqual(flattenBindings([
        {label: 'Junk', accelerators: ['', 'Disabled', '<Super>', '<Mod4>d', null, 42]},
        {label: 'Repeated', accelerators: ['<Super>space', '<super><Shift>space']},
        // The same binding under two names is one taken binding: the first name describes it.
        {label: 'Again', accelerators: ['<Shift><Super>space']},
        {accelerators: ['<Super>e']},
        null,
        'nonsense',
        {label: 'No list'},
    ]), [
        {accelerator: '<Super>space', label: 'Repeated'},
        {accelerator: '<Shift><Super>space', label: 'Repeated'},
        {accelerator: '<Super>e', label: '<Super>e'},
    ]);
});

test('flattenBindings of nothing usable is an empty list', () => {
    for (const nothing of [undefined, null, [], 'nonsense', {}, 42])
        assert.deepEqual(flattenBindings(nothing), []);
});

test('a settings key name reads as a label', () => {
    assert.equal(prettifyKeyName('switch-to-workspace-1'), 'Switch to workspace 1');
    assert.equal(prettifyKeyName('screenshots'), 'Screenshots');
    assert.equal(prettifyKeyName('toggle-message-tray'), 'Toggle message tray');
    assert.equal(prettifyKeyName('switch_input_source'), 'Switch input source');
    assert.equal(prettifyKeyName('-leading-dash'), 'Leading dash');
});

test('a settings key name that says nothing becomes an empty label', () => {
    for (const nothing of ['', '   ', null, undefined, 42, {}])
        assert.equal(prettifyKeyName(nothing), '');
});

test('a binding is named by its label, else by its accelerator', () => {
    assert.equal(bindingLabel({accelerator: '<Super>d', label: 'Show desktop'}), 'Show desktop');
    assert.equal(bindingLabel({accelerator: '<Super>d'}), 'Super+D');
    assert.equal(bindingLabel('<Primary>space'), 'Ctrl+Space');
    assert.equal(bindingLabel({accelerator: '<Super>', label: ''}), null);
    assert.equal(bindingLabel('Disabled'), null);
    assert.equal(bindingLabel(null), null);
    assert.equal(bindingLabel(42), null);
});

test('a collision can be described without knowing its shape', () => {
    const existing = flattenBindings([
        {label: 'Switch input source', accelerators: ['<Super>space']},
        {label: 'Show desktop', accelerators: ['<Super>d']},
    ]);

    const clash = collides('<Super>space', {existing});
    assert.equal(bindingLabel(clash), 'Switch input source');
});

test('the default summon shortcut is the schema default', () => {
    assert.equal(normalizeAccelerator('<Super>space'), '<Super>space');
    assert.equal(acceleratorLabel('<Super>space'), 'Super+Space');
});
