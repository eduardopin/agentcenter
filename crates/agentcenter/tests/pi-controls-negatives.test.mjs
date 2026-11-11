// Negative paths of the Pi control contract, through the REAL pieces the GUI
// client depends on: the extension entry point (extensions/pi-control/index.ts,
// session_start -> listener), the private Unix socket, the discovery manifest
// and the wire protocol the Rust client (control::pi) speaks. Only the Pi
// session context and the model provider are synthetic; no network, no real
// model, no existing session. Needs no Pi SDK.
//
//   node --test crates/agentcenter/tests/pi-controls-negatives.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, readdir, rm } from 'node:fs/promises';
import { createConnection } from 'node:net';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const extensionDir = fileURLToPath(new URL('../../../extensions/pi-control/', import.meta.url));
const { privateBase } = await import(pathToFileURL(join(extensionDir, 'test-support.mjs')).href);
const { default: piControl } = await import(pathToFileURL(join(extensionDir, 'index.ts')).href);
globalThis.fetch = () => { throw new Error('NETWORK FORBIDDEN'); };

const DEADLINE = 60_000; // operations.mjs generation deadline, not configurable
const SECRET = 'synthetic-private-provider-detail';
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));

// The 60 s generation deadline is a literal in the extension; the harness
// shortens exactly that timer so the real abort path runs in 150 ms.
function shortenDeadline() {
  const real = globalThis.setTimeout;
  globalThis.setTimeout = (fn, ms, ...rest) => real(fn, ms === DEADLINE ? 150 : ms, ...rest);
  return () => { globalThis.setTimeout = real; };
}

async function rig() {
  const root = await mkdtemp(`${privateBase}/ac-neg-`);
  const runtimeDir = join(root, 'runtime');
  await mkdir(runtimeDir, { mode: 0o700 });
  const saved = { runtime: process.env.XDG_RUNTIME_DIR, state: process.env.XDG_STATE_HOME };
  process.env.XDG_RUNTIME_DIR = runtimeDir;
  process.env.XDG_STATE_HOME = root;
  let providerCalls = 0, release, lastSignal;
  const provider = { // controllable model: each call parks until the test settles it
    calls: () => providerCalls,
    signal: () => lastSignal,
    settle: (overrides = {}) => release({ role: 'assistant', stopReason: 'stop',
      content: [{ type: 'text', text: 'Objective: continue. Next step: test.' }],
      usage: { cost: { total: 0 } }, ...overrides }),
  };
  const modelRegistry = {
    getProviderAuthStatus: () => ({ configured: true }),
    streamSimple: (_model, _context, options) => {
      providerCalls++; lastSignal = options.signal;
      return { result: () => new Promise(resolve => { release = resolve; }) };
    },
  };
  const sessionId = 'neg-session', sessionFile = '/synthetic/neg.jsonl';
  const state = () => ({ managerId: 'manager', revision: '1', sessionId, sessionFile, leafId: 'leaf' });
  const ctx = {
    cwd: root, model: { provider: 'fake', id: 'fake' }, modelRegistry,
    compactGuardedCapability: 'agentcenter.compactGuarded.v1',
    compactGuarded: async () => { throw new Error('not used'); },
    isIdle: () => true, hasPendingMessages: () => false,
    sessionManager: { getGuardedCompactionState: state, getSessionId: () => sessionId,
      getSessionFile: () => sessionFile, getLeafId: () => 'leaf',
      buildSessionProjection: () => ({ messages: [{ role: 'user', content: 'Work request' }] }) },
  };
  const handlers = new Map();
  piControl({ on: (event, handler) => handlers.set(event, handler), registerCommand() {} });
  await handlers.get('session_start')({}, ctx);
  const [dir] = (await readdir(runtimeDir)).filter(name => name.startsWith('ac-'));
  assert.ok(dir, 'extension published a discovery directory');
  const manifestFile = join(runtimeDir, dir, 'manifest.json');
  const manifest = JSON.parse(await readFile(manifestFile, 'utf8'));
  const base = { protocol_version: 2, session_id: manifest.session_id, session_file: manifest.session_file,
    pid: manifest.pid, process_starttime: manifest.process_starttime,
    instance_nonce: manifest.instance_nonce, expected: manifest.expected };
  const send = body => new Promise((resolve, reject) => {
    const client = createConnection(manifest.socket);
    let text = '';
    client.on('connect', () => client.end(JSON.stringify(body) + '\n'));
    client.on('data', chunk => { text += chunk; });
    client.on('end', () => { try { resolve(JSON.parse(text)); } catch (error) { reject(error); } });
    client.on('error', reject);
  });
  const artifacts = async () => await readdir(manifest.handoff_directory).catch(() => []);
  // Same shape as the Rust client: poll status with the ORIGINAL id and snapshot.
  async function until(id, accept) {
    for (let i = 0; i < 200; i++) {
      const reply = await send({ ...base, operation_id: id, action: 'status' });
      if (accept(reply)) return reply;
      await wait(10);
    }
    assert.fail(`operation ${id} never reached the expected state`);
  }
  const terminal = id => until(id, r => !['accepted', 'running', 'unknown'].includes(r.state));
  async function parked(id) { // provider reached and awaiting settle
    for (let i = 0; i < 200 && providerCalls === 0; i++) await wait(10);
    assert.equal(providerCalls, 1, 'provider was called once');
    assert.equal((await send({ ...base, operation_id: id, action: 'status' })).state, 'running');
  }
  return { base, send, provider, artifacts, terminal, parked, until, manifest,
    async close() {
      await handlers.get('session_shutdown')({}, ctx);
      process.env.XDG_RUNTIME_DIR = saved.runtime;
      if (saved.state === undefined) delete process.env.XDG_STATE_HOME; else process.env.XDG_STATE_HOME = saved.state;
      await rm(root, { recursive: true, force: true });
    } };
}

test('quota/rejection: provider 429 surfaces as failed/model_error, no detail leaks, no artifact', { timeout: 15000 }, async () => {
  const r = await rig();
  try {
    const id = randomUUID();
    const accepted = await r.send({ ...r.base, operation_id: id, action: 'handoff' });
    assert.equal(accepted.state, 'accepted');
    await r.parked(id);
    r.provider.settle({ stopReason: 'error', errorMessage: `429 quota ${SECRET}`, content: [] });
    const result = await r.terminal(id);
    assert.equal(result.state, 'failed');
    assert.equal(result.code, 'model_error');
    assert.equal(JSON.stringify(result).includes(SECRET), false, 'provider detail must not cross the socket');
    assert.equal(result.artifact_path, undefined);
    assert.deepEqual(await r.artifacts(), []);
    assert.equal(r.provider.calls(), 1, 'a failure is never retried behind the client');
    // The rejection class of the wire itself: a wrong nonce is refused before any effect.
    const denied = await r.send({ ...r.base, instance_nonce: '0'.repeat(64), operation_id: randomUUID(), action: 'handoff' });
    assert.equal(denied.state, 'rejected');
    assert.equal(denied.code, 'unauthorized');
    assert.equal(r.provider.calls(), 1);
  } finally { await r.close(); }
});

test('cancel during polling: running -> cancelled, provider signal aborted, no artifact', { timeout: 15000 }, async () => {
  const r = await rig();
  try {
    const id = randomUUID();
    await r.send({ ...r.base, operation_id: id, action: 'handoff' });
    await r.parked(id);
    const cancel = await r.send({ ...r.base, operation_id: id, action: 'cancel' });
    assert.ok(['running', 'cancelled'].includes(cancel.state), `cancel reply ${cancel.state}`);
    const result = await r.terminal(id);
    assert.equal(result.state, 'cancelled');
    assert.equal(result.code, 'cancelled');
    assert.equal(r.provider.signal().aborted, true, 'cancel reaches the provider call');
    assert.equal(result.artifact_path, undefined);
    assert.deepEqual(await r.artifacts(), []);
    // A late provider answer after the cancel must not resurrect or publish.
    r.provider.settle();
    await wait(100);
    assert.deepEqual(await r.send({ ...r.base, operation_id: id, action: 'status' }), result);
    assert.deepEqual(await r.artifacts(), []);
    // The cancel is idempotent and the tombstone stays terminal.
    assert.equal((await r.send({ ...r.base, operation_id: id, action: 'cancel' })).state, 'cancelled');
  } finally { await r.close(); }
});

test('timeout: the generation deadline ends the operation as cancelled/timeout with no artifact', { timeout: 15000 }, async () => {
  const restore = shortenDeadline();
  const r = await rig();
  try {
    const id = randomUUID();
    await r.send({ ...r.base, operation_id: id, action: 'handoff' });
    // The provider never answers; only the deadline can end the operation.
    const result = await r.terminal(id);
    assert.equal(result.state, 'cancelled');
    assert.equal(result.code, 'timeout');
    assert.equal(r.provider.signal().aborted, true);
    assert.equal(result.artifact_path, undefined);
    assert.deepEqual(await r.artifacts(), []);
    assert.equal(r.provider.calls(), 1);
    // The busy flag is released: the session accepts a NEW operation afterwards.
    const next = randomUUID();
    assert.equal((await r.send({ ...r.base, operation_id: next, action: 'handoff' })).state, 'accepted');
    // Let the operation reach the provider before teardown: shutting down in the
    // accept-to-start window races the manifest refresh (reported as a finding).
    for (let i = 0; i < 200 && r.provider.calls() < 2; i++) await wait(10);
    assert.equal(r.provider.calls(), 2);
  } finally { restore(); await r.close(); }
});

test('same-ID query: status and replays return the stored reply, never a second effect', { timeout: 15000 }, async () => {
  const r = await rig();
  try {
    const id = randomUUID();
    await r.send({ ...r.base, operation_id: id, action: 'handoff' });
    await r.parked(id);
    r.provider.settle();
    const done = await r.terminal(id);
    assert.equal(done.state, 'completed');
    assert.equal((await r.artifacts()).length, 1);
    // Status polling, and a repeated identical dispatch, replay the same reply.
    assert.deepEqual(await r.send({ ...r.base, operation_id: id, action: 'status' }), done);
    assert.deepEqual(await r.send({ ...r.base, operation_id: id, action: 'handoff' }), done);
    assert.equal(r.provider.calls(), 1);
    assert.equal((await r.artifacts()).length, 1);
    // Same ID, other action or other snapshot: refused, not reinterpreted.
    const otherAction = await r.send({ ...r.base, operation_id: id, action: 'compact' });
    assert.equal(otherAction.code, 'duplicate_mismatch');
    const otherLeaf = await r.send({ ...r.base, expected: { ...r.base.expected, leafId: 'other' }, operation_id: id, action: 'status' });
    assert.equal(otherLeaf.code, 'duplicate_mismatch');
    // An ID the extension never saw is unknown, not completed and not an error.
    const stranger = await r.send({ ...r.base, operation_id: randomUUID(), action: 'status' });
    assert.equal(stranger.state, 'unknown');
    assert.equal(stranger.code, 'unknown_operation');
    assert.equal(r.provider.calls(), 1);
  } finally { await r.close(); }
});

test('late result after timeout: the answer arrives after the deadline and changes nothing', { timeout: 15000 }, async () => {
  const restore = shortenDeadline();
  const r = await rig();
  try {
    const id = randomUUID();
    await r.send({ ...r.base, operation_id: id, action: 'handoff' });
    const timedOut = await r.terminal(id);
    assert.equal(timedOut.code, 'timeout');
    // The provider answers successfully AFTER the deadline.
    r.provider.settle();
    await wait(200);
    assert.deepEqual(await r.send({ ...r.base, operation_id: id, action: 'status' }), timedOut,
      'the late success must not rewrite the terminal reply');
    assert.deepEqual(await r.artifacts(), [], 'the late success must not publish a handoff');
    // Replaying the dispatch with the same ID also stays terminal, no restart.
    assert.deepEqual(await r.send({ ...r.base, operation_id: id, action: 'handoff' }), timedOut);
    assert.equal(r.provider.calls(), 1);
  } finally { restore(); await r.close(); }
});
