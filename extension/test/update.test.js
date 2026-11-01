import {test} from 'node:test';
import assert from 'node:assert/strict';

import {
    DATA_SUBDIR,
    INSTALLED_VERSION_FILE,
    LATEST_VERSION_FILE,
    UPDATE_NOTIFICATION_TITLE,
    UpdateState,
    compareVersions,
    dataDir,
    parseVersion,
    updateNotification,
    updateStatus,
    versionFilePaths,
} from '../lib/update.js';

// The files the installer writes, and the values a real install leaves in them.
const INSTALLED = '0.1.0\n';
const LATEST = '0.2.0\n';

test('parses the version line the installer writes', () => {
    assert.deepEqual(parseVersion('0.1.0\n'), {version: '0.1.0', major: 0, minor: 1, patch: 0});
    assert.deepEqual(parseVersion('1.2.3'), {version: '1.2.3', major: 1, minor: 2, patch: 3});
    assert.deepEqual(parseVersion('v2.0.1'), {version: '2.0.1', major: 2, minor: 0, patch: 1});
    assert.deepEqual(parseVersion('  0.10.0  '), {version: '0.10.0', major: 0, minor: 10, patch: 0});
    // Git-describe style suffixes and short forms are tolerated, the numeric core is what counts.
    assert.deepEqual(parseVersion('0.2.0-3-gabc1234'), {version: '0.2.0', major: 0, minor: 2, patch: 0});
    assert.deepEqual(parseVersion('1.2'), {version: '1.2.0', major: 1, minor: 2, patch: 0});
    assert.deepEqual(parseVersion(3), {version: '3.0.0', major: 3, minor: 0, patch: 0});
    // A version file may carry extra lines; the first non-empty one is the version.
    assert.deepEqual(parseVersion('\n0.3.0\narch=x86_64\n'), {
        version: '0.3.0', major: 0, minor: 3, patch: 0,
    });
});

test('junk is unparseable rather than a made-up version', () => {
    for (const junk of [null, undefined, '', '   ', 'latest', 'v', '1.x.0', '0.1.0 broken thing',
        {}, [], 'x0.1.0', {version: '1.0.0'}])
        assert.equal(parseVersion(junk), null, `${JSON.stringify(junk)} must not parse`);
});

test('compares versions numerically, not as strings', () => {
    assert.equal(compareVersions('0.1.0', '0.2.0'), -1);
    assert.equal(compareVersions('0.2.0', '0.1.0'), 1);
    assert.equal(compareVersions('0.1.0', '0.1.0'), 0);
    assert.equal(compareVersions('0.10.0', '0.9.0'), 1, '0.10 is newer than 0.9');
    assert.equal(compareVersions('1.0.0', '0.99.99'), 1);
    // Missing components count as zero, a suffix does not change the core.
    assert.equal(compareVersions('1.2', '1.2.0'), 0);
    assert.equal(compareVersions('1.2.1-rc1', '1.2.1'), 0);
    assert.equal(compareVersions('nonsense', '1.0.0'), null);
    assert.equal(compareVersions('1.0.0', ''), null);
});

test('a newer release in the latest file is surfaced', () => {
    const status = updateStatus(INSTALLED, LATEST);
    assert.equal(status.state, UpdateState.AVAILABLE);
    assert.equal(status.available, true);
    assert.equal(status.current, '0.1.0');
    assert.equal(status.latest, '0.2.0');
    assert.match(status.message, /0\.2\.0/);
    assert.match(status.message, /0\.1\.0/);
    assert.match(status.message, /installer/i);
});

test('the same or an older latest file says nothing at all', () => {
    for (const latest of ['0.1.0\n', '0.0.9\n', 'v0.1.0']) {
        const status = updateStatus(INSTALLED, latest);
        assert.equal(status.state, UpdateState.CURRENT, latest);
        assert.equal(status.available, false);
        assert.equal(status.message, '');
    }
});

test('a missing or unreadable version file degrades to unknown, never a false alarm', () => {
    for (const [installed, latest] of [[INSTALLED, null], [null, LATEST], [null, null],
        ['', LATEST], [INSTALLED, 'not a version']]) {
        const status = updateStatus(installed, latest);
        assert.equal(status.state, UpdateState.UNKNOWN);
        assert.equal(status.available, false);
        assert.equal(status.message, '');
    }
});

test('the notification is only produced when there is something to say', () => {
    assert.deepEqual(updateNotification(updateStatus(INSTALLED, LATEST)), {
        title: UPDATE_NOTIFICATION_TITLE,
        body: updateStatus(INSTALLED, LATEST).message,
    });
    assert.equal(updateNotification(updateStatus(INSTALLED, INSTALLED)), null);
    assert.equal(updateNotification(updateStatus(null, null)), null);
    assert.equal(updateNotification(null), null);
    assert.equal(updateNotification(undefined), null);
});

test('the version files live in the XDG data directory the installer uses', () => {
    const paths = versionFilePaths({xdgDataHome: '/data', home: '/home/me'});
    assert.equal(paths.installed, `/data/${DATA_SUBDIR}/${INSTALLED_VERSION_FILE}`);
    assert.equal(paths.latest, `/data/${DATA_SUBDIR}/${LATEST_VERSION_FILE}`);

    // No XDG_DATA_HOME (or a relative one) falls back to ~/.local/share, like every other
    // Lumen store (contract §12, §13).
    for (const xdgDataHome of [null, '', 'relative/path']) {
        const fallback = versionFilePaths({xdgDataHome, home: '/home/me'});
        assert.equal(fallback.installed, `/home/me/.local/share/${DATA_SUBDIR}/${INSTALLED_VERSION_FILE}`);
        assert.equal(fallback.latest, `/home/me/.local/share/${DATA_SUBDIR}/${LATEST_VERSION_FILE}`);
        assert.equal(dataDir({xdgDataHome, home: '/home/me'}), `/home/me/.local/share/${DATA_SUBDIR}`);
    }

    assert.equal(dataDir({xdgDataHome: '/data'}), `/data/${DATA_SUBDIR}`);
    assert.equal(dataDir({}), null, 'no home and no data dir at all');
});
