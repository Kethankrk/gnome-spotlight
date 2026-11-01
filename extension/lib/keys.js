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
};

export const DEFAULT_PAGE_SIZE = 8;

/**
 * The intent of a key press, or null for a key the list does not use. `pageSize` is accepted so
 * callers can pass one options object; the navigator owns the step it actually takes.
 * Ticket 14 grows the section-aware Tab intents.
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
        return shift ? 'prev' : 'next';
    case KEYS.IsoLeftTab:
        return 'prev';
    default:
        return null;
    }
}
