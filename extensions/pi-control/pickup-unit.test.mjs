// CC5 pickup edges at operation level: real listener and dispatch, fake Pi
// context and fake command context (newSession/withSession are spies). Each
// test owns a fresh per-process registry. Provider is fake; network forbidden.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createConnection } from 'node:net';
import { randomUUID } from 'node:crypto';
import { createOperations, PICKUP_CAPACITY, pickupMessage } from './operations.mjs';
import { listenPrivate } from './transport.mjs';

const P = process.env.PI_SDK_DIR;
assert.ok(P, 'Set PI_SDK_DIR to the original read-only Pi SDK');
const { ModelRuntime } = await import(`${P}/dist/core/model-runtime.js`);
const { ModelRegistry } = await import(`${P}/dist/core/model-registry.js`);
const { AuthStorage } = await import(`${P}/dist/core/auth-storage.js`);
const { createAssistantMessageEventStream } = await import(`${P}/node_modules/@earendil-works/pi-ai/dist/index.js`);
globalThis.fetch = () => { throw new Error('NETWORK FORBIDDEN'); };

const REGISTRY = Symbol.for('agentcenter.pi-control.pickups.v1');
// Shared with crates/agentcenter-core/src/control/pi_tests.rs: the Rust client
// validates exactly these bytes. Regenerate with AGENTCENTER_WRITE_FIXTURES=1.
const FIXTURE = resolve(import.meta.dirname, '../../crates/agentcenter-core/tests/fixtures/pi/arm_refusals.json');
const OP = '12345678-1234-1234-1234-123456789abc';
const SESSION = { id: 'test', file: '/private/session.jsonl' };
const EXPECTED = { managerId: 'manager', revision: '1', sessionId: SESSION.id, sessionFile: SESSION.file, leafId: 'leaf' };
const sleep = ms => new Promise(done => setTimeout(done, ms));

function request(socket, body) {
  return new Promise((done, reject) => {
    const client = createConnection(socket); let text = '';
    client.on('connect', () => client.end(JSON.stringify(body) + '\n'));
    client.on('data', chunk => { text += chunk; });
    client.on('end', () => { try { done(JSON.parse(text)); } catch (error) { reject(error); } });
    client.on('error', reject);
  });
}

async function world() {
  delete globalThis[REGISTRY];
  const root = await mkdtemp(`/run/user/${process.getuid()}/ac-pu-`);
  const runtimeDir = join(root, 'runtime'); await mkdir(runtimeDir, { mode: 0o700 });
  const handoffs = join(root, 'handoffs'); await mkdir(handoffs, { mode: 0o700 });
  const w = { root, handoffs, revision: '1', busy: false, guarded: true, model: true, calls: 0, newSession: 0, sent: [], onChange: undefined };
  const runtime = await ModelRuntime.create({ credentials: AuthStorage.inMemory(), modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
  runtime.registerProvider('cc5-unit', { baseUrl: 'http://invalid.invalid', apiKey: 'synthetic', api: 'cc5-unit',
    models: [{ id: 'fake', name: 'fake', input: ['text'], reasoning: false, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 2048 }],
    streamSimple(model) {
      w.calls++;
      const stream = createAssistantMessageEventStream();
      stream.push({ type: 'done', reason: 'stop', message: { role: 'assistant', content: [{ type: 'text', text: 'Synthetic handoff. Next: continue.' }],
        api: model.api, provider: model.provider, model: model.id, stopReason: 'stop', timestamp: Date.now(),
        usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } } });
      stream.end();
      return stream;
    } });
  const registry = new ModelRegistry(runtime);
  const model = registry.find('cc5-unit', 'fake');
  const ctx = {
    cwd: root, modelRegistry: registry,
    get model() { return w.model ? model : undefined; },
    get compactGuardedCapability() { return w.guarded ? 'agentcenter.compactGuarded.v1' : undefined; },
    compactGuarded: async () => { throw new Error('compact is not part of these tests'); },
    isIdle: () => !w.busy, hasPendingMessages: () => false,
    sessionManager: {
      getGuardedCompactionState: () => ({ ...EXPECTED, revision: w.revision }),
      getSessionId: () => SESSION.id, getSessionFile: () => SESSION.file,
      buildSessionProjection: () => ({ messages: [{ role: 'user', content: 'Work request' }] }),
    },
  };
  let listener;
  w.ops = await createOperations({ getContext: () => ctx, outputRoot: handoffs,
    onChange: async current => { w.onChange?.(current); await listener?.refresh(current); } });
  listener = await listenPrivate(runtimeDir, w.ops.metadata, w.ops.dispatch, w.ops.pickupCapability);
  const { session_id, session_file, pid, process_starttime, instance_nonce } = w.ops.metadata();
  w.registry = () => globalThis[REGISTRY];
  w.body = (action, { protocol_version = 3, operation_id = OP, expected = EXPECTED, ...rest } = {}) =>
    ({ protocol_version, operation_id, action, session_id, session_file, pid, process_starttime, instance_nonce, expected: { ...expected }, ...rest });
  w.send = (action, options) => request(listener.socket, w.body(action, options));
  // Fake ExtensionCommandContext: newSession hands a fake replacement to withSession.
  w.command = ({ parent, sendFails = false, hang = false } = {}) => ({ hasUI: false,
    newSession: async ({ parentSession, withSession }) => {
      w.newSession++;
      const entries = [], header = { parentSession: parent ?? parentSession };
      const replacement = { sessionManager: { getSessionId: () => 'next', getSessionFile: () => '/private/next.jsonl', getHeader: () => header, getEntries: () => entries },
        sendUserMessage: async text => {
          w.sent.push(text);
          if (sendFails) throw new Error('synthetic send failure');
          entries.push({ type: 'message', id: 'entry1', message: { role: 'user', content: text } });
        } };
      if (hang) { w.late = () => withSession(replacement); return new Promise(() => {}); }
      await withSession(replacement);
      return { cancelled: false };
    } });
  w.synthetic = count => {
    for (let i = 0; i < count; i++) w.registry().ops.set(randomUUID(), { state: 'accepted', code: 'armed', instance: 'synthetic', armed: Date.now() });
  };
  w.close = async () => {
    // Let the arm's asynchronous manifest refresh land before removing its directory.
    await sleep(20);
    await listener.close(); await w.ops.close();
    delete globalThis[REGISTRY];
    await rm(root, { recursive: true, force: true });
  };
  return w;
}

test('arm refusals: real replies carry metadata and no record, and match the Rust fixture', { timeout: 10000 }, async t => {
  const scenarios = {
    busy: async w => { w.busy = true; return w.send('handoff_pickup'); },
    stale_revision: async w => w.send('handoff_pickup', { expected: { ...EXPECTED, revision: '0' } }),
    disabled: async w => { w.model = false; return w.send('handoff_pickup'); },
    unsupported_guarded_compaction: async w => { w.guarded = false; return w.send('handoff_pickup'); },
    capacity: async w => { w.synthetic(PICKUP_CAPACITY); return w.send('handoff_pickup'); },
    duplicate_mismatch: async w => {
      assert.equal((await w.send('handoff_pickup')).code, 'armed');
      return w.send('handoff_pickup', { expected: { ...EXPECTED, leafId: 'other' } });
    },
    expired: async w => {
      assert.equal((await w.send('handoff_pickup')).code, 'armed');
      assert.equal((await w.send('cancel')).code, 'cancelled');
      t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
      t.mock.timers.tick(30 * 60 * 1000 + 1);
      try { return await w.send('handoff_pickup'); } finally { t.mock.timers.reset(); }
    },
  };
  const replies = {};
  for (const [name, scenario] of Object.entries(scenarios)) {
    const w = await world();
    try {
      const reply = await scenario(w);
      assert.equal(reply.protocol_version, 3, name);
      assert.equal(reply.state, name === 'expired' ? 'unknown' : 'rejected', name);
      assert.equal(reply.code, name, name);
      assert.equal(Object.hasOwn(reply, 'pickup'), false, `${name}: a refusal carries no record`);
      assert.equal(reply.metadata.capabilities.handoff_pickup.protocol_version, 3, name);
      assert.equal(w.calls, 0, `${name}: no effect`);
      reply.metadata.handoff_directory = '/private/handoffs';
      replies[name] = reply;
    } finally { await w.close(); }
  }
  if (process.env.AGENTCENTER_WRITE_FIXTURES === '1') await writeFile(FIXTURE, JSON.stringify(replies, null, 2) + '\n');
  assert.deepEqual(replies, JSON.parse(await readFile(FIXTURE, 'utf8')));
});

test('protocol 2 stays protocol 2: no pickup capability, no v3 echo, no arming', async () => {
  const w = await world();
  try {
    const v2 = await w.send('status', { protocol_version: 2 });
    assert.deepEqual([v2.protocol_version, v2.state, v2.code], [2, 'unknown', 'unknown_operation']);
    assert.equal(Object.hasOwn(v2.metadata.capabilities, 'handoff_pickup'), false);
    const v3 = await w.send('status');
    assert.deepEqual([v3.protocol_version, v3.metadata.capabilities.handoff_pickup.protocol_version], [3, 3]);
    // A v2 handoff_pickup is refused as incompatible and never armed.
    const arm = await w.send('handoff_pickup', { protocol_version: 2 });
    assert.deepEqual([arm.protocol_version, arm.state, arm.code], [2, 'rejected', 'incompatible']);
    assert.equal(w.registry()?.ops.has(OP) ?? false, false);
    assert.equal(w.registry()?.active, undefined);
    // Listener refusals before dispatch echo the request's protocol as well.
    const real = w.body('status').instance_nonce;
    const wrong = real.replace(/^./, c => (c === '0' ? '1' : '0'));
    const unauthorized2 = await w.send('status', { protocol_version: 2, instance_nonce: wrong });
    const unauthorized3 = await w.send('status', { instance_nonce: wrong });
    assert.deepEqual([unauthorized2.protocol_version, unauthorized2.code], [2, 'unauthorized']);
    assert.deepEqual([unauthorized3.protocol_version, unauthorized3.code], [3, 'unauthorized']);
  } finally { await w.close(); }
});

test('arm TTL from the environment can shorten the window but never extend it', async t => {
  const w = await world();
  const ttl = process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS;
  try {
    process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS = String(10 * 15_000);
    assert.equal((await w.send('handoff_pickup')).code, 'armed');
    t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
    t.mock.timers.tick(15_001);
    const status = w.ops.dispatch(w.body('status'));
    assert.deepEqual([status.state, status.code], ['rejected', 'arm_expired']);
    await w.ops.pickup(OP, w.command());
    assert.deepEqual([w.calls, w.newSession], [0, 0]);
  } finally {
    t.mock.timers.reset();
    if (ttl === undefined) delete process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS; else process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS = ttl;
    await w.close();
  }
});

test('parent mismatch: the new session is not loaded with the handoff', async () => {
  const w = await world();
  try {
    assert.equal((await w.send('handoff_pickup')).code, 'armed');
    await w.ops.pickup(OP, w.command({ parent: '/private/other.jsonl' }));
    const status = w.ops.dispatch(w.body('status'));
    assert.deepEqual([status.state, status.code, status.pickup.stage], ['failed', 'parent_mismatch', 'new_session_confirmed']);
    assert.equal(status.pickup.new_session.parent_session, '/private/other.jsonl');
    assert.deepEqual(w.sent, []);
    assert.equal(status.pickup.loaded_entry_id, null);
  } finally { await w.close(); }
});

test('revision moving after the handoff is persisted stops before newSession', async () => {
  const w = await world();
  try {
    assert.equal((await w.send('handoff_pickup')).code, 'armed');
    // generate() publishes in its finally, after its last precondition and
    // before the command checks again: the session moves exactly there.
    w.onChange = () => { if (w.registry().active?.state === 'running') w.revision = '2'; };
    await w.ops.pickup(OP, w.command());
    const status = w.ops.dispatch(w.body('status'));
    assert.deepEqual([status.state, status.code, status.pickup.stage], ['rejected', 'stale_revision', 'handoff_persisted']);
    assert.equal(w.calls, 1);
    assert.equal(w.newSession, 0);
    // The persisted handoff stays (the resume route).
    assert.equal((await readdir(w.handoffs)).includes(status.pickup.handoff.path.split('/').at(-1)), true);
  } finally { await w.close(); }
});

test('registry capacity: finished records are evicted oldest first, unfinished ones never', async () => {
  const w = await world();
  try {
    const a = randomUUID(), b = randomUUID(), c = randomUUID();
    w.synthetic(PICKUP_CAPACITY - 1);
    assert.equal((await w.send('handoff_pickup', { operation_id: a })).code, 'armed', 'the 128th record fits');
    assert.equal((await w.send('cancel', { operation_id: a })).code, 'cancelled');
    assert.equal(w.registry().ops.size, PICKUP_CAPACITY);
    // Full, one finished: it is evicted to make room.
    assert.equal((await w.send('handoff_pickup', { operation_id: b })).code, 'armed');
    assert.equal(w.registry().ops.size, PICKUP_CAPACITY);
    assert.equal(w.registry().ops.has(a), false);
    // The evicted ID is just unknown now; its late command does nothing.
    assert.equal((await w.send('status', { operation_id: a })).code, 'unknown_operation');
    await w.ops.pickup(a, w.command());
    assert.deepEqual([w.calls, w.newSession], [0, 0]);
    // Full of unfinished records: refused, nothing recorded, nothing evicted.
    w.registry().ops.delete(b); w.registry().active = undefined;
    w.synthetic(1);
    const before = [...w.registry().ops.keys()];
    const refused = await w.send('handoff_pickup', { operation_id: c });
    assert.deepEqual([refused.state, refused.code], ['rejected', 'capacity']);
    assert.deepEqual([...w.registry().ops.keys()], before);
  } finally { await w.close(); }
});

test('sendUserMessage failure is reported as pickup_failed, not swallowed', { timeout: 15000 }, async () => {
  const w = await world();
  try {
    assert.equal((await w.send('handoff_pickup')).code, 'armed');
    const started = Date.now();
    await w.ops.pickup(OP, w.command({ sendFails: true }));
    const status = w.ops.dispatch(w.body('status'));
    assert.deepEqual([status.state, status.code, status.pickup.stage, status.pickup.loaded_entry_id], ['failed', 'pickup_failed', 'pickup_sent', null]);
    assert.equal(w.sent.length, 1);
    assert.ok(Date.now() - started < 5000, 'failure is observed, not timed out');
  } finally { await w.close(); }
});

test('stuck newSession times out: the process is free again and a late load settles the record', { timeout: 10000 }, async () => {
  const w = await world();
  const timeout = process.env.AGENTCENTER_PI_PICKUP_NEW_SESSION_TIMEOUT_MS;
  try {
    process.env.AGENTCENTER_PI_PICKUP_NEW_SESSION_TIMEOUT_MS = '50';
    assert.equal((await w.send('handoff_pickup')).code, 'armed');
    await w.ops.pickup(OP, w.command({ hang: true }));
    const status = w.ops.dispatch(w.body('status'));
    assert.deepEqual([status.state, status.code, status.pickup.stage], ['unknown', 'new_session_unconfirmed', 'clearing']);
    assert.equal(w.registry().active, undefined);
    assert.equal(w.ops.metadata().capabilities.handoff.disabled_reason, null);
    const next = randomUUID();
    assert.equal((await w.send('handoff_pickup', { operation_id: next })).code, 'armed');
    await w.send('cancel', { operation_id: next });
    // Unknown is not terminal: evidence arriving late still settles it.
    await w.late();
    const settled = w.ops.dispatch(w.body('status'));
    assert.deepEqual([settled.state, settled.code, settled.pickup.loaded_entry_id], ['completed', 'pickup_loaded', 'entry1']);
    assert.equal(w.sent[0], pickupMessage(settled.pickup.handoff.path, settled.pickup.handoff.sha256));
    await sleep(0);
  } finally {
    if (timeout === undefined) delete process.env.AGENTCENTER_PI_PICKUP_NEW_SESSION_TIMEOUT_MS; else process.env.AGENTCENTER_PI_PICKUP_NEW_SESSION_TIMEOUT_MS = timeout;
    await w.close();
  }
});
