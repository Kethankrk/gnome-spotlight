// Where a key press should move the selection. Keyvals are plain numbers (Gdk/Clutter key
// symbols), so the mapping is testable without a shell.

export const KEYS = {
    Up: 0xff52,
    Down: 0xff54,
    Home: 0xff50,
    End: 0xff57,
    PageUp: 0xff55,
    PageDown: 0xff56,
    Return: 0xff0d,
    KpEnter: 0xff8d,
    Tab: 0xff09,
    IsoLeftTab: 0xfe20,
    Delete: 0xffff,
};

export const DEFAULT_PAGE_SIZE = 8;

// `Clutter.ModifierType.SHIFT_MASK`, spelled as the number for the same reason the keyvals are:
// on Wayland Shift+Tab arrives as the Tab keyval with this bit set, so the modifier is the only
// thing that distinguishes it from Tab.
export const SHIFT_MASK = 1 << 0;

/** Whether Shift is held in a Clutter (`Clutter.ModifierType`) modifier mask. */
export function hasShift(modifiers) {
    return (Number(modifiers ?? 0) & SHIFT_MASK) !== 0;
}

/**
 * The intent of a key press, or null for a key the list does not use. `pageSize` is accepted so
 * callers can pass one options object; the navigator owns the step it actually takes. Tab and
 * Shift+Tab move between sections (ticket 14); `next`/`prev` stay the row-by-row movement the
 * arrows use. `delete` is not navigation: it is the key that forgets the selected row (story 48),
 * and what a modifier does with it is the shell's to decide.
 */
export function intentFor(keyval, options = {}) {
    const shift = options?.shift ?? false;

    switch (keyval) {
    case KEYS.Down:
        return 'next';
    case KEYS.Up:
        return 'prev';
    case KEYS.Home:
        return 'first';
    case KEYS.End:
        return 'last';
    case KEYS.PageDown:
        return 'page-down';
    case KEYS.PageUp:
        return 'page-up';
    case KEYS.Return:
    case KEYS.KpEnter:
        return 'activate';
    case KEYS.Tab:
        return shift ? 'prev-section' : 'next-section';
    case KEYS.IsoLeftTab:
        return 'prev-section';
    case KEYS.Delete:
        return 'delete';
    default:
        return null;
    }
}
