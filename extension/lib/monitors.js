// Multi-monitor placement (ticket 15): which monitor the overlay belongs on, and where the card
// sits inside it. Pure arithmetic over plain `{x,y,width,height}` rectangles — no gi:// imports,
// so `node --test` exercises exactly what the shell runs. `Main.layoutManager` supplies the
// rectangles (`.monitors` / `.primaryMonitor`) and `global.get_pointer()` the pointer.

/** Is the point inside the monitor's rectangle? Bounds are half-open on the right/bottom edge. */
function contains(monitor, point) {
    return typeof monitor?.x === 'number' && typeof monitor?.y === 'number'
        && typeof monitor?.width === 'number' && typeof monitor?.height === 'number'
        && point.x >= monitor.x && point.x < monitor.x + monitor.width
        && point.y >= monitor.y && point.y < monitor.y + monitor.height;
}

function isPoint(pointer) {
    return pointer !== null && typeof pointer === 'object'
        && Number.isFinite(pointer.x) && Number.isFinite(pointer.y);
}

function sameRect(a, b) {
    return typeof b === 'object' && b !== null
        && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
}

/** The index of the monitor the primary names, by index or by rectangle, or -1. */
function primaryIndex(monitors, primary) {
    if (primary === null || primary === undefined)
        return -1;

    if (typeof primary === 'number') {
        return Number.isInteger(primary) && primary >= 0 && primary < monitors.length
            ? primary
            : -1;
    }

    if (typeof primary === 'object') {
        const identity = monitors.indexOf(primary);
        return identity >= 0 ? identity : monitors.findIndex(monitor => sameRect(monitor, primary));
    }

    return -1;
}

/**
 * The index of the monitor the overlay should open on: the one containing the pointer, else the
 * primary monitor, else the first monitor. Returns -1 when there is no monitor at all — the caller
 * then has nothing to place the overlay on and should leave it where it is.
 *
 * `monitors` are plain `{x,y,width,height}` rectangles, `pointer` is `{x,y}` or null, and `primary`
 * is the monitor object `Main.layoutManager.primaryMonitor` returns (or its index).
 */
export function pickMonitor(monitors, pointer, {primary = null} = {}) {
    if (!Array.isArray(monitors) || monitors.length === 0)
        return -1;

    if (isPoint(pointer)) {
        const under = monitors.findIndex(monitor => contains(monitor, pointer));
        if (under >= 0)
            return under;
    }

    const fallback = primaryIndex(monitors, primary);
    return fallback >= 0 ? fallback : 0;
}

/**
 * The screen position of a `size`d card centred on `monitor`. Absolute coordinates, so the caller
 * subtracts the monitor origin when the card lives inside a monitor-sized actor. Whole pixels; a
 * card larger than its monitor simply hangs outside it, and missing input degrades to the origin.
 */
export function centeredOrigin(monitor, size = {}) {
    if (!Number.isFinite(monitor?.x) || !Number.isFinite(monitor?.y)
        || !Number.isFinite(monitor?.width) || !Number.isFinite(monitor?.height))
        return {x: 0, y: 0};

    const width = Number.isFinite(size?.width) ? size.width : 0;
    const height = Number.isFinite(size?.height) ? size.height : 0;

    // `|| 0` folds a -0 (odd leftover) back to 0 so the result never trips an identity comparison.
    return {
        x: monitor.x + (Math.round((monitor.width - width) / 2) || 0),
        y: monitor.y + (Math.round((monitor.height - height) / 2) || 0),
    };
}
