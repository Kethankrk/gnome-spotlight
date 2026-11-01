// Wire ⇄ model. A contract §3 result row is an `a{sv}`: GJS hands every field back as a
// `GLib.Variant` (deep_unpack() stops at a `v` member), so the row is unpacked by hand here
// rather than in the shell glue.

export function unpack(value) {
    if (value === null || value === undefined)
        return value;
    if (typeof value.deep_unpack === 'function')
        return unpack(value.deep_unpack());
    if (Array.isArray(value))
        return value.map(unpack);
    if (typeof value === 'object') {
        const plain = {};
        for (const [key, item] of Object.entries(value))
            plain[key] = unpack(item);
        return plain;
    }
    return value;
}

/** One result row, in the shape `lib/viewmodel.js` and the overlay expect. */
export function resultFromRow(row) {
    const fields = unpack(row) ?? {};
    return {
        id: fields.id ?? '',
        provider: fields.provider ?? '',
        title: fields.title ?? '',
        subtitle: fields.subtitle ?? '',
        icon: fields.icon ?? '',
        score: Number(fields.score ?? 0),
        actions: fields.actions ?? [],
        preview: fields.preview ?? {},
    };
}

export function resultsFromRows(rows) {
    return (rows ?? []).map(resultFromRow);
}
