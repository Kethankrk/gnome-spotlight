// The daemon handshake, as a pure function of what D-Bus tells us: does something own the
// engine's bus name, and does the protocol version it reports match ours? No gi:// imports, so
// `node --test` exercises exactly the logic the shell runs.

import {SUPPORTED_MAJOR} from './protocol.js';

export const EngineStatus = {
    OK: 'ok',
    NOT_RUNNING: 'not-running',
    VERSION_MISMATCH: 'version-mismatch',
};

// The exact human wording the overlay shows when the engine is absent or crashed.
export const ENGINE_NOT_RUNNING = 'engine not running';

function major(version) {
    if (version === null || version === undefined || version === '')
        return null;
    const parsed = Number(version);
    return Number.isFinite(parsed) ? parsed : null;
}

export function engineState({nameOwner = null, version = null, supported = SUPPORTED_MAJOR} = {}) {
    const remote = major(version);
    if (!nameOwner || remote === null)
        return {status: EngineStatus.NOT_RUNNING, message: ENGINE_NOT_RUNNING};

    const ours = major(supported);
    if (remote === ours)
        return {status: EngineStatus.OK, message: `engine ready (protocol ${remote})`};

    return {
        status: EngineStatus.VERSION_MISMATCH,
        message: `engine protocol ${remote} does not match extension protocol ${ours}`,
    };
}
