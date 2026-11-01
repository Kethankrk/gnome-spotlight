// Accessibility and appearance policy for the overlay, kept pure so `node --test` covers the
// decisions and the shell glue only reads settings and applies the result (contract §18, ticket
// 14). Two things live here: whether motion is allowed, and what a screen reader is told.
//
// The preferences are GNOME's own. Reduced motion is the newer, explicit
// `org.gnome.desktop.a11y.interface reduced-motion` ("no-preference" or "reduce"); the global switch
// it supersedes is `org.gnome.desktop.interface enable-animations`, which spells the same intent as
// a boolean and is still the key the shell and every other app honour. Both are read, so a user who
// only knows one of them is respected. High contrast lives in the a11y schema next to them.

import {PROVIDER_LABELS} from './protocol.js';

export const A11Y_SCHEMA = 'org.gnome.desktop.a11y.interface';
export const INTERFACE_SCHEMA = 'org.gnome.desktop.interface';
export const ANIMATIONS_KEY = 'enable-animations';
export const REDUCED_MOTION_KEY = 'reduced-motion';
export const HIGH_CONTRAST_KEY = 'high-contrast';
export const REDUCED_MOTION_OFF = 'no-preference';

// Accessible names for the parts of the overlay that carry no text of their own.
export const CARD_ACCESSIBLE_NAME = 'Lumen';
export const ENTRY_ACCESSIBLE_NAME = 'Search';
export const RESULTS_ACCESSIBLE_NAME = 'Results';
export const PREVIEW_ACCESSIBLE_NAME = 'Preview';
export const SECTION_FALLBACK_ACCESSIBLE_NAME = 'Results';
export const ROW_FALLBACK_ACCESSIBLE_NAME = 'Result';

// The style class the high-contrast theme turns on; it sits on the backdrop, like the appearance
// classes, so the stylesheet reaches the card and everything in it.
export const HIGH_CONTRAST_CLASS = 'lumen-high-contrast';

/**
 * The two booleans the overlay actually applies, from what GSettings answered. A key that could
 * not be read arrives as `undefined` and leaves GNOME's default in place rather than inventing a
 * preference: motion is on, high contrast is off. `reduce` is accepted as well as `true` so a
 * caller may hand over either spelling of reduced motion.
 */
export function accessibilityState({enableAnimations, reducedMotion, highContrast} = {}) {
    const reduced = reducedMotion === true || reducedMotion === 'reduce';

    return {
        animationsEnabled: enableAnimations !== false && !reduced,
        highContrast: highContrast === true,
    };
}

/** Whether an animation may run. Reduced motion and high contrast both suppress it. */
export function shouldAnimate({animationsEnabled, highContrast} = {}) {
    return animationsEnabled !== false && highContrast !== true;
}

/**
 * A row's accessible name, built from what the row shows. The subtitle carries the detail a
 * screen reader would otherwise miss — a file's folder, a toggle's state — and the provider is the
 * fallback for a row that somehow has neither.
 */
export function accessibleNameFor(result = {}) {
    const title = text(result?.title);
    const subtitle = text(result?.subtitle);

    if (title && subtitle)
        return `${title}, ${subtitle}`;
    if (title)
        return title;
    if (subtitle)
        return subtitle;

    return providerName(result?.provider) ?? ROW_FALLBACK_ACCESSIBLE_NAME;
}

/** A section header's accessible name: its label, else its provider's label. */
export function sectionAccessibleName(section = {}) {
    return text(section?.label)
        || providerName(section?.provider)
        || SECTION_FALLBACK_ACCESSIBLE_NAME;
}

function providerName(provider) {
    if (typeof provider !== 'string' || provider === '')
        return null;

    return PROVIDER_LABELS[provider] ?? provider;
}

function text(value) {
    if (value === undefined || value === null)
        return '';

    return String(value).trim();
}
