import {test} from 'node:test';
import assert from 'node:assert/strict';

import {EngineStatus, engineState, ENGINE_NOT_RUNNING} from '../lib/engine.js';
import {SUPPORTED_MAJOR} from '../lib/protocol.js';

const OWNER = ':1.42';

test('no name owner means the engine is not running', () => {
    const state = engineState({nameOwner: null, version: null, supported: SUPPORTED_MAJOR});
    assert.equal(state.status, EngineStatus.NOT_RUNNING);
    assert.equal(state.message, 'engine not running');
    assert.equal(state.message, ENGINE_NOT_RUNNING);
});

test('an owner that has not reported a version is still not usable', () => {
    const state = engineState({nameOwner: OWNER, version: null, supported: SUPPORTED_MAJOR});
    assert.equal(state.status, EngineStatus.NOT_RUNNING);
    assert.match(state.message, /engine not running/i);
});

test('an unparseable version degrades to not-running rather than mismatching', () => {
    const state = engineState({nameOwner: OWNER, version: 'wat', supported: SUPPORTED_MAJOR});
    assert.equal(state.status, EngineStatus.NOT_RUNNING);
});

test('a matching version means the handshake succeeded and names the version', () => {
    const state = engineState({
        nameOwner: OWNER, version: SUPPORTED_MAJOR, supported: SUPPORTED_MAJOR,
    });
    assert.equal(state.status, EngineStatus.OK);
    assert.match(state.message, new RegExp(String(SUPPORTED_MAJOR)));
});

test('a differing version is a mismatch that names both versions', () => {
    const state = engineState({nameOwner: OWNER, version: 2, supported: SUPPORTED_MAJOR});
    assert.equal(state.status, EngineStatus.VERSION_MISMATCH);
    assert.match(state.message, /2/);
    assert.match(state.message, new RegExp(String(SUPPORTED_MAJOR)));
    assert.match(state.message, /mismatch|does not match/i);
});

test('supported defaults to the extension protocol major', () => {
    assert.equal(engineState({nameOwner: OWNER, version: 1}).status, EngineStatus.OK);
    assert.equal(engineState({nameOwner: OWNER, version: 2}).status, EngineStatus.VERSION_MISMATCH);
});

test('the state is a plain serialisable record of status and message', () => {
    const state = engineState({nameOwner: OWNER, version: SUPPORTED_MAJOR});
    assert.deepEqual(Object.keys(state).sort(), ['message', 'status']);
    assert.equal(typeof state.message, 'string');
    assert.ok(Object.values(EngineStatus).includes(state.status));
});
