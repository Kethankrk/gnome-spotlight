// GTK accelerators, as far as they can be reasoned about without GTK.
//
// The preferences window (extension/prefs.js) captures a key combination, stores it in
// `summon-shortcut`, and warns when the combination is already used by the shell or by
// gnome-settings-daemon. Capture itself needs GTK (`Gtk.accelerator_name`), but everything that
// decides *what was chosen* and *whether something else already owns it* lives here, so it is
// Node-tested (`extension/test/shortcut.test.js`) instead of only exercisable in a running shell.
//
// The rules mirror GTK 4.20 (`gtk_accelerator_parse` / `gtk_accelerator_name`), verified on the
// build host:
//
//   * A modifier is written `<Name>`; names are case-insensitive and `Ctrl`/`Primary` both mean
//     `Control`. `Mod1`…`Mod5`, `Win` and `Cmd` are **not** modifiers GTK accepts, so an
//     accelerator containing one is not an accelerator at all.
//   * `gtk_accelerator_name()` prints modifiers in the order Shift, Control, Alt, Super, Hyper,
//     Meta, whatever order they were written in — `MODIFIER_ORDER` below is that order, so a
//     normalised accelerator is byte-identical to what GTK would have produced.
//   * The key part is looked up case-sensitively: `space` parses, `SPACE` does not; `Tab` parses,
//     `tab` does not; single letters are spelled lowercase (`t`, not `T`).
//
// Consequences the rest of this file implements: single letters are lowercased (turning `<Super>T`,
// which GTK cannot use, into a usable `<Super>t`), other key names are kept exactly as spelled (so
// a value GTK accepts is never turned into one it rejects), and *comparison* ignores case, so two
// spellings of one key are still reported as the one collision they are.
//
// Pure JS: no `gi://` imports, so `node --test` loads it.

/** Modifier names in the order GTK prints them (`gtk_accelerator_name`). */
export const MODIFIER_ORDER = ['Shift', 'Control', 'Alt', 'Super', 'Hyper', 'Meta'];

/** The spelling a modifier is displayed with — the one GTK's own labels use. */
export const MODIFIER_LABELS = {
    Shift: 'Shift',
    Control: 'Ctrl',
    Alt: 'Alt',
    Super: 'Super',
    Hyper: 'Hyper',
    Meta: 'Meta',
};

/** What a modifier may be called in an accelerator, and the GTK name it means. */
export const MODIFIER_ALIASES = {
    control: 'Control',
    ctrl: 'Control',
    primary: 'Control',
    alt: 'Alt',
    shift: 'Shift',
    super: 'Super',
    hyper: 'Hyper',
    meta: 'Meta',
};

/** The key names whose GTK spelling is not what a user should read. */
export const KEY_LABELS = {
    space: 'Space',
    page_up: 'Page Up',
    page_down: 'Page Down',
};

/** The spelling GNOME uses for "no shortcut at all". */
export const DISABLED_ACCELERATOR = 'Disabled';

const DISABLED = new Set(['', 'disabled', 'none']);
const SINGLE_LETTER = /^[A-Za-z]$/;

/**
 * The pieces of an accelerator: the modifiers in GTK's order, the key name as spelled, and the
 * normalised accelerator these two make (`<Shift><Super>space`). `null` when `value` is not an
 * accelerator a GTK application could bind — an empty string, `Disabled`, a bare modifier, an
 * unknown modifier, a malformed `<…`.
 */
export function parseAccelerator(value) {
    if (typeof value !== 'string')
        return null;

    let rest = value.trim();
    if (DISABLED.has(rest.toLowerCase()))
        return null;

    const modifiers = [];
    while (rest.startsWith('<')) {
        const end = rest.indexOf('>');
        if (end < 0 || !addModifier(modifiers, rest.slice(1, end)))
            return null;
        rest = rest.slice(end + 1).trim();
    }

    // The human spelling (`Ctrl+Alt+T`, what the preferences window shows) names its modifiers
    // with `+` separators instead. No key name contains a `+`, so this cannot confuse the two.
    let key = rest;
    if (key.includes('+')) {
        const tokens = key.split('+').map(token => token.trim()).filter(token => token !== '');
        if (tokens.length === 0)
            return null;

        key = tokens.pop();
        for (const token of tokens) {
            if (!addModifier(modifiers, token))
                return null;
        }
    }

    key = normaliseKey(key);
    if (key === null)
        return null;

    const ordered = MODIFIER_ORDER.filter(modifier => modifiers.includes(modifier));
    return {
        modifiers: ordered,
        key,
        accelerator: ordered.map(modifier => `<${modifier}>`).join('') + key,
    };
}

/** The normalised accelerator for `value`, or `null` when it is not a usable one. */
export function normalizeAccelerator(value) {
    return parseAccelerator(value)?.accelerator ?? null;
}

/**
 * The accelerator as a person reads it: `Super+Space`, `Ctrl+Alt+T` — the wording GTK's own
 * `gtk_accelerator_get_label()` uses. `null` when `value` is not an accelerator.
 */
export function acceleratorLabel(value) {
    const parsed = parseAccelerator(value);
    if (!parsed)
        return null;

    const modifiers = parsed.modifiers.map(modifier => MODIFIER_LABELS[modifier]);
    return [...modifiers, keyLabel(parsed.key)].join('+');
}

/**
 * Whether two accelerators name the same binding. Spelling, modifier order and aliases do not
 * matter — and neither does the case of the key, because two spellings of one key are one
 * collision even though GTK's keyval lookup is case-sensitive. Two unusable accelerators are
 * never equal.
 */
export function sameAccelerator(a, b) {
    const left = normalizeAccelerator(a);
    const right = normalizeAccelerator(b);
    return left !== null && right !== null && left.toLowerCase() === right.toLowerCase();
}

/**
 * The first entry in `existing` that already uses `accel`, or `null` when it is free. `existing`
 * entries are accelerators (`'<Super>space'`) or `{accelerator, label}` bindings; the matching
 * entry is handed back as it was passed, so the caller can name it with `bindingLabel`. An
 * accelerator that is not usable, or an entry that is not, cannot collide.
 */
export function collides(accel, {existing = []} = {}) {
    const wanted = normalizeAccelerator(accel);
    if (wanted === null || !Array.isArray(existing))
        return null;

    const needle = wanted.toLowerCase();
    for (const entry of existing) {
        const taken = normalizeAccelerator(acceleratorOf(entry));
        if (taken !== null && taken.toLowerCase() === needle)
            return entry;
    }

    return null;
}

/**
 * The taken bindings of one or more settings, as `{accelerator, label}` entries ready for
 * `collides`. `bindings` are the gathered groups — `{label, accelerators}`, where `accelerators`
 * is what `get_strv()` returned for a settings key. Unusable accelerators (`Disabled`, an empty
 * string, a modifier-only value) are dropped, and one accelerator is one entry however many keys
 * or groups name it: the first label is the one that describes it.
 */
export function flattenBindings(bindings) {
    if (!Array.isArray(bindings))
        return [];

    const flattened = [];
    const seen = new Set();

    for (const group of bindings) {
        if (!group || typeof group !== 'object')
            continue;

        const label = typeof group.label === 'string' ? group.label.trim() : '';
        const accelerators = Array.isArray(group.accelerators) ? group.accelerators : [];

        for (const value of accelerators) {
            const parsed = parseAccelerator(value);
            if (!parsed)
                continue;

            const canonical = parsed.accelerator.toLowerCase();
            if (seen.has(canonical))
                continue;

            seen.add(canonical);
            flattened.push({
                accelerator: parsed.accelerator,
                label: label !== '' ? label : parsed.accelerator,
            });
        }
    }

    return flattened;
}

/** How to name a binding in a message: its label, else its accelerator, else nothing. */
export function bindingLabel(entry) {
    if (typeof entry === 'string')
        return acceleratorLabel(entry);

    if (!entry || typeof entry !== 'object')
        return null;

    const label = typeof entry.label === 'string' ? entry.label.trim() : '';
    return label !== '' ? label : acceleratorLabel(entry.accelerator);
}

/**
 * A settings key name read as a label: `switch-to-workspace-1` → `Switch to workspace 1`. The
 * shell's and the desktop's binding schemas hold no human name for a key, so this is what a
 * collision message can honestly call the binding that owns the accelerator.
 */
export function prettifyKeyName(name) {
    if (typeof name !== 'string')
        return '';

    const words = name.replace(/[-_]+/g, ' ').trim().split(/\s+/).filter(word => word !== '');
    if (words.length === 0)
        return '';

    const text = words.join(' ');
    return text.charAt(0).toUpperCase() + text.slice(1);
}

function addModifier(modifiers, token) {
    const modifier = MODIFIER_ALIASES[token.trim().toLowerCase()];
    if (!modifier)
        return false;

    if (!modifiers.includes(modifier))
        modifiers.push(modifier);
    return true;
}

// A key name as GTK spells it. Single letters are lowercase there (`t`) and would otherwise make
// the accelerator unusable; named keys (`F12`, `Print`, `Tab`, `Page_Up`) keep the case the caller
// wrote, because GTK's own lookup is case-sensitive and we have no keyval table to correct with.
function normaliseKey(key) {
    const trimmed = key.trim();
    if (trimmed === '' || /[<>\s]/.test(trimmed))
        return null;

    if (MODIFIER_ALIASES[trimmed.toLowerCase()])
        return null;

    return SINGLE_LETTER.test(trimmed) ? trimmed.toLowerCase() : trimmed;
}

function keyLabel(key) {
    const named = KEY_LABELS[key.toLowerCase()];
    if (named)
        return named;

    if (/^[A-Za-z0-9]$/.test(key))
        return key.toUpperCase();

    return key.charAt(0).toUpperCase() + key.slice(1);
}

function acceleratorOf(entry) {
    if (typeof entry === 'string')
        return entry;

    if (entry && typeof entry === 'object')
        return entry.accelerator;

    return null;
}
