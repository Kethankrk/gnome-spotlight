// The update path, as a pure function of two version strings (ticket 16).
//
// Lumen is updated by re-running the installer, so there is no self-updater and no network call
// (contract §9): the installer writes the version it installed to
// `$XDG_DATA_HOME/lumen/installed-version` and the newest published release it knows about to
// `$XDG_DATA_HOME/lumen/latest-version`. The extension reads both and tells the user when the
// second one is newer; deciding *that* is this module, so `node --test` covers it and
// `extension.js` only does the file reads and the `Main.notify` call.
//
// No gi:// imports, like every `extension/lib/*.js` model module.

/** The sub-directory of the XDG data directory the installer and the daemon share. */
export const DATA_SUBDIR = 'lumen';

/** What the installer placed here: the version currently on disk. */
export const INSTALLED_VERSION_FILE = 'installed-version';

/** The newest published release the installer last heard about. */
export const LATEST_VERSION_FILE = 'latest-version';

export const UpdateState = {
    /** Same version, or the latest file is older than what is installed. */
    CURRENT: 'current',
    /** A newer release exists; the user should re-run the installer. */
    AVAILABLE: 'available',
    /** Not installed, or a version file is missing/unreadable. Say nothing. */
    UNKNOWN: 'unknown',
};

export const UPDATE_NOTIFICATION_TITLE = 'Lumen update available';

const VERSION_LINE = /^[vV]?(\d+)(?:\.(\d+))?(?:\.(\d+))?(?:[-+][0-9A-Za-z.+-]+)?$/;

/**
 * The first non-empty line of a version file, as `{version, major, minor, patch}`, or null when it
 * is not a version at all. Numbers are accepted too, so a caller may pass either shape.
 */
export function parseVersion(text) {
    if (typeof text === 'number' && Number.isFinite(text))
        text = String(text);

    if (typeof text !== 'string')
        return null;

    const line = text
        .split('\n')
        .map(part => part.trim())
        .find(part => part !== '');

    if (line === undefined)
        return null;

    const match = VERSION_LINE.exec(line);
    if (!match)
        return null;

    const [, major, minor = '0', patch = '0'] = match;
    return {
        version: `${Number(major)}.${Number(minor)}.${Number(patch)}`,
        major: Number(major),
        minor: Number(minor),
        patch: Number(patch),
    };
}

/** `-1`, `0` or `1` — or null when either side is not a version. */
export function compareVersions(left, right) {
    const a = parseVersion(left);
    const b = parseVersion(right);
    if (!a || !b)
        return null;

    for (const part of ['major', 'minor', 'patch']) {
        if (a[part] !== b[part])
            return a[part] < b[part] ? -1 : 1;
    }
    return 0;
}

/**
 * `installed` and `latest` are the *contents* of the two files (or null when a file is absent).
 * The record is plain and serialisable: `{state, available, current, latest, message}`.
 */
export function updateStatus(installed, latest) {
    const current = parseVersion(installed);
    const published = parseVersion(latest);
    const base = {
        state: UpdateState.UNKNOWN,
        available: false,
        current: current?.version ?? null,
        latest: published?.version ?? null,
        message: '',
    };

    if (!current || !published)
        return base;

    if (compareVersions(current.version, published.version) < 0)
        return {
            state: UpdateState.AVAILABLE,
            available: true,
            current: current.version,
            latest: published.version,
            message: `Lumen ${published.version} is available (you have ${current.version}). ` +
                'Re-run the Lumen installer to update.',
        };

    return {...base, state: UpdateState.CURRENT};
}

/** The `Main.notify(title, body)` arguments, or null when there is nothing to announce. */
export function updateNotification(status) {
    if (!status?.available || !status.message)
        return null;

    return {title: UPDATE_NOTIFICATION_TITLE, body: status.message};
}

/**
 * The Lumen data directory, following the XDG base directory spec exactly as the daemon's stores
 * do: `$XDG_DATA_HOME` when it is set, non-empty and absolute, else `$HOME/.local/share`. Null when
 * neither is known.
 */
export function dataDir({xdgDataHome = null, home = null} = {}) {
    const xdg = typeof xdgDataHome === 'string' && xdgDataHome.startsWith('/') ? xdgDataHome : null;
    const base = xdg ?? (home ? `${home}/.local/share` : null);
    return base === null ? null : `${base}/${DATA_SUBDIR}`;
}

/** Absolute path of one of the version files, or null when there is no data directory. */
export function versionFilePath(file, options = {}) {
    const dir = dataDir(options);
    return dir === null ? null : `${dir}/${file}`;
}

/** Both paths, in one call, so the shell reads exactly what the installer wrote. */
export function versionFilePaths(options = {}) {
    return {
        installed: versionFilePath(INSTALLED_VERSION_FILE, options),
        latest: versionFilePath(LATEST_VERSION_FILE, options),
    };
}
