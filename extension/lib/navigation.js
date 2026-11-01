// Selection over one flat list of rows (row 0 is the Top Hit, then the sections in order).
// Arrow keys wrap; the paging keys clamp so that holding PageDown parks on the last row. Tab and
// Shift+Tab move between the sections the view model produced, so a long list is reachable without
// walking every row (contract §7, §18).

import {DEFAULT_PAGE_SIZE} from './keys.js';

export {DEFAULT_PAGE_SIZE};

export function createNavigator(count = 0, {
    pageSize = DEFAULT_PAGE_SIZE,
    onChange = null,
    sections = null,
} = {}) {
    const total = Math.max(0, Math.trunc(count) || 0);
    const fallbackStep = Math.max(1, Math.trunc(pageSize) || DEFAULT_PAGE_SIZE);
    const boundaries = normaliseSections(sections, total);
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

    // The last boundary at or before `index`; `current` is always inside the list, so this is the
    // section the selected row belongs to.
    function sectionOf(index) {
        let found = 0;
        for (let i = 0; i < boundaries.length; i++) {
            if (boundaries[i] > index)
                break;
            found = i;
        }
        return found;
    }

    const navigator = {
        get count() {
            return total;
        },
        get current() {
            return current;
        },
        get sections() {
            return [...boundaries];
        },
        onChange,

        next() {
            return moveTo(total > 0 ? (current + 1) % total : -1);
        },
        prev() {
            return moveTo(total > 0 ? (current - 1 + total) % total : -1);
        },
        nextSection() {
            if (total === 0)
                return moveTo(-1);
            return moveTo(boundaries[(sectionOf(current) + 1) % boundaries.length]);
        },
        prevSection() {
            if (total === 0)
                return moveTo(-1);
            const previous = (sectionOf(current) - 1 + boundaries.length) % boundaries.length;
            return moveTo(boundaries[previous]);
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

/**
 * The row indices at which each displayed section starts, from a view model
 * (`{topHit, sections, flat}` — contract §7b). The Top Hit is a section of its own, ahead of the
 * labelled ones, so Tab reaches it first, and an empty section is skipped exactly as the overlay
 * renders it. This is what a caller hands to `createNavigator` as `sections`.
 */
export function sectionStarts(view = {}) {
    const starts = [];
    let index = 0;

    if (view?.topHit) {
        starts.push(0);
        index = 1;
    }

    for (const section of view?.sections ?? []) {
        const items = section?.items ?? [];
        if (items.length === 0)
            continue;

        starts.push(index);
        index += items.length;
    }

    return starts;
}

// Section starts the navigator can actually stop on: integers inside the list, deduped and sorted,
// and always including row 0 so `prevSection` from the first section has somewhere to land.
function normaliseSections(sections, total) {
    if (total <= 0)
        return [];

    const given = Array.isArray(sections)
        ? [...new Set(sections.filter(index => Number.isInteger(index) && index >= 0 && index < total))]
            .sort((a, b) => a - b)
        : [];

    // Without section information every row is its own section, which keeps Tab's list-level
    // meaning for a caller that has nothing better to go on.
    if (given.length === 0)
        return Array.from({length: total}, (_value, index) => index);

    if (given[0] !== 0)
        given.unshift(0);

    return given;
}

function clamp(value, low, high) {
    return Math.min(high, Math.max(low, value));
}
