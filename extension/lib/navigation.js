// Selection over one flat list of rows (row 0 is the Top Hit, then the sections in order).
// Arrow keys wrap; the paging keys clamp so that holding PageDown parks on the last row.

import {DEFAULT_PAGE_SIZE} from './keys.js';

export {DEFAULT_PAGE_SIZE};

export function createNavigator(count = 0, {pageSize = DEFAULT_PAGE_SIZE, onChange = null} = {}) {
    const total = Math.max(0, Math.trunc(count) || 0);
    const fallbackStep = Math.max(1, Math.trunc(pageSize) || DEFAULT_PAGE_SIZE);
    let current = total > 0 ? 0 : -1;

    function moveTo(next) {
        if (next === current)
            return current;

        current = next;
        navigator.onChange?.(current);
        return current;
    }

    function step(size) {
        return Math.max(1, Math.trunc(size) || fallbackStep);
    }

    const navigator = {
        get count() {
            return total;
        },
        get current() {
            return current;
        },
        onChange,

        next() {
            return moveTo(total > 0 ? (current + 1) % total : -1);
        },
        prev() {
            return moveTo(total > 0 ? (current - 1 + total) % total : -1);
        },
        first() {
            return moveTo(total > 0 ? 0 : -1);
        },
        last() {
            return moveTo(total > 0 ? total - 1 : -1);
        },
        pageUp(size) {
            return moveTo(clamp(current - step(size), 0, total - 1));
        },
        pageDown(size) {
            return moveTo(clamp(current + step(size), 0, total - 1));
        },
        select(index) {
            if (!Number.isInteger(index) || index < 0 || index >= total)
                return current;
            return moveTo(index);
        },
    };

    return navigator;
}

function clamp(value, low, high) {
    return Math.min(high, Math.max(low, value));
}
