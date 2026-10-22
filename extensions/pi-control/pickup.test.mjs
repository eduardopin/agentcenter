// CC5 contract: handoff_pickup armed over the socket (protocol 3) and executed
// only by the registered command, against the private CC0 SDK runtime
// (createAgentSessionRuntime: real newSession teardown/rebind/withSession).
// The command arrives by session.prompt, the same path the TUI editor uses when
// Herdr types it. Provider is fake; network is forbidden.
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, readdir, rm, stat } from 'node:fs/promises';
import { readdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { createConnection } from 'node:net';
import { createHash, randomUUID } from 'node:crypto';
import { pickupMessage } from './operations.mjs';

const SDK = process.env.AGENTCENTER_GUARDED_PI_SDK;
assert.ok(SDK, 'Set AGENTCENTER_GUARDED_PI_SDK to a prepared private runtime');
const EXTENSION = resolve(import.meta.dirname, 'index.ts');
const { createAgentSession, createAgentSessionRuntime, ModelRuntime, AuthStorage, SessionManager, SettingsManager, DefaultResourceLoader } =
  Object.assign({}, ...await Promise.all(['sdk', 'agent-session-runtime', 'model-runtime', 'auth-storage', 'session-manager', 'settings-manager', 'resource-loader']
    .map(name => import(`${SDK}/dist/core/${name}.js`))));
const { createAssistantMessageEventStream } = await import(`${SDK}/node_modules/@earendil-works/pi-ai/dist/index.js`);
globalThis.fetch = () => { throw new Error('NETWORK FORBIDDEN'); };

const HANDOFF_TEXT = 'AGENTCENTER-CANARY-cc5 Verified handoff. Next: continue.';
const REGISTRY = Symbol.for('agentcenter.pi-control.pickups.v1');
const TERMINAL = ['completed', 'rejected', 'failed', 'cancelled', 'unknown'];
const sleep = ms => new Promise(done => setTimeout(done, ms));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
function request(socket, body) {
  return new Promise((done, reject) => {
    const client = createConnection(socket); let text = '';
    client.on('connect', () => client.end(JSON.stringify(body) + '\n'));
    client.on('data', chunk => { text += chunk; });
    client.on('end', () => { try { done(JSON.parse(text)); } catch (error) { reject(error); } });
    client.on('error', reject);
  });
}
function assistant(model, text, overrides = {}) {
  return { role: 'assistant', content: [{ type: 'text', text }], api: model.api, provider: model.provider, model: model.id, stopReason: 'stop', timestamp: Date.now(),
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, ...overrides };
}

// One private runtime per test: own XDG dirs, own runtime/session directory.
async function harness({ cancelSwitch = false } = {}) {
  const root = await mkdtemp(`/run/user/${process.getuid()}/ac-pk-`);
  const runtimeDir = join(root, 'runtime'); await mkdir(runtimeDir, { mode: 0o700 });
  const previous = { XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR, XDG_STATE_HOME: process.env.XDG_STATE_HOME };
  process.env.XDG_RUNTIME_DIR = runtimeDir; process.env.XDG_STATE_HOME = root;
  const handoffs = join(root, 'agentcenter', 'handoffs');
  const h = { root, runtimeDir, handoffs, calls: { handoff: 0, turn: 0 }, errors: [],
    // Provider behaviour per call kind; tests replace these.
    onHandoff: (model, stream) => { stream.push({ type: 'done', reason: 'stop', message: assistant(model, HANDOFF_TEXT) }); stream.end(); },
    onTurn: (model, stream) => { stream.push({ type: 'done', reason: 'stop', message: assistant(model, 'Synthetic turn.') }); stream.end(); },
    spies: { newSession: 0, fork: 0, switchSession: 0, navigateTree: 0, reload: 0, shutdown: 0, beforeNewSession: [] } };
  const modelRuntime = await ModelRuntime.create({ credentials: AuthStorage.inMemory(), modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
  modelRuntime.registerProvider('cc5-fake', { baseUrl: 'http://invalid.invalid', apiKey: 'synthetic', api: 'cc5-fake',
    models: [{ id: 'fake', name: 'fake', input: ['text'], reasoning: false, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 2048 }],
    streamSimple(model, context) {
      const stream = createAssistantMessageEventStream();
      const handoff = context.messages.length === 1 && typeof context.messages[0].content === 'string' && context.messages[0].content.startsWith('Create a work handoff');
      if (handoff) { h.calls.handoff++; h.onHandoff(model, stream); } else { h.calls.turn++; h.onTurn(model, stream, context); }
      return stream;
    } });
  const settingsManager = SettingsManager.inMemory({ compaction: { enabled: true, keepRecentTokens: 256, reserveTokens: 1024 } });
  h.settingsManager = settingsManager;
  h.compactionBefore = structuredClone(settingsManager.getCompactionSettings());
  const model = modelRuntime.getModel('cc5-fake', 'fake');
  const testExtension = pi => pi.on('session_before_switch', () => (cancelSwitch ? { cancel: true } : undefined));
  const factory = async ({ cwd, agentDir, sessionManager, sessionStartEvent }) => {
    const resourceLoader = new DefaultResourceLoader({ cwd, agentDir, settingsManager, additionalExtensionPaths: [EXTENSION], extensionFactories: [testExtension],
      noSkills: true, noThemes: true, noPromptTemplates: true, noContextFiles: true });
    await resourceLoader.reload();
    const result = await createAgentSession({ cwd, agentDir, modelRuntime, model, sessionManager, settingsManager, resourceLoader, noTools: 'all', sessionStartEvent });
    return { ...result, services: { cwd, agentDir, modelRuntime, settingsManager, resourceLoader, diagnostics: [] }, diagnostics: [] };
  };
  const manager = SessionManager.create(root, join(root, 'sessions'));
  for (let i = 0; i < 3; i++) {
    manager.appendMessage({ role: 'user', content: `Synthetic goal ${i}`, timestamp: Date.now() });
    manager.appendMessage(assistant(model, 'Synthetic response.', { usage: { input: 10, output: 2, cacheRead: 0, cacheWrite: 0, totalTokens: 12, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } }));
  }
  const runtime = await createAgentSessionRuntime(factory, { cwd: root, agentDir: root, sessionManager: manager });
  h.runtime = runtime;
  // Same shape as Pi print/rpc hosts: newSession goes through the runtime host.
  // Every other replacement/termination action is a counting spy.
  const bind = async () => runtime.session.bindExtensions({
    commandContextActions: {
      waitForIdle: () => runtime.session.waitForIdle(),
      newSession: async options => {
        h.spies.newSession++;
        const record = globalThis[REGISTRY]?.active;
        const files = readdirSync(handoffs).filter(name => name.endsWith('.md'));
        h.spies.beforeNewSession.push({ stage: record?.stage, code: record?.code, handoff: record?.handoff && { ...record.handoff },
          files: files.map(name => ({ path: join(handoffs, name), sha256: sha256(readFileSync(join(handoffs, name))) })), options: { parentSession: options?.parentSession } });
        return runtime.newSession(options);
      },
      fork: async () => { h.spies.fork++; return { cancelled: true }; },
      navigateTree: async () => { h.spies.navigateTree++; return { cancelled: true }; },
      switchSession: async () => { h.spies.switchSession++; return { cancelled: true }; },
      reload: async () => { h.spies.reload++; },
    },
    shutdownHandler: () => { h.spies.shutdown++; },
    onError: error => h.errors.push(error),
  });
  runtime.setRebindSession(bind);
  await bind();
  h.manifest = async () => {
    const dirs = await readdir(runtimeDir);
    assert.equal(dirs.length, 1, 'exactly one live endpoint');
    return JSON.parse(await readFile(join(runtimeDir, dirs[0], 'manifest.json'), 'utf8'));
  };
  h.body = async (action, operation_id = randomUUID(), protocol_version = 3) => {
    const { socket, capabilities, handoff_directory, model_auth, ...identity } = await h.manifest();
    return { socket, body: { ...identity, protocol_version, operation_id, action } };
  };
  h.arm = async (operation_id = randomUUID()) => {
    const { socket, body } = await h.body('handoff_pickup', operation_id);
    return { socket, body, reply: await request(socket, body) };
  };
  h.status = async (operation_id, action = 'status') => {
    const { socket, body } = await h.body(action, operation_id);
    return request(socket, body);
  };
  h.deliver = text => runtime.session.prompt(text);
  h.close = async () => {
    try { await runtime.dispose(); } catch { /* already torn down */ }
    for (const [key, value] of Object.entries(previous)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
    await rm(root, { recursive: true, force: true });
  };
  return h;
}
function assertNoReplacement(h) {
  assert.deepEqual({ fork: h.spies.fork, switchSession: h.spies.switchSession, navigateTree: h.spies.navigateTree, reload: h.spies.reload, shutdown: h.spies.shutdown },
    { fork: 0, switchSession: 0, navigateTree: 0, reload: 0, shutdown: 0 });
  // Auto-compaction semantics untouched: same setting, same thresholds, no compaction entry.
  assert.deepEqual(h.settingsManager.getCompactionSettings(), h.compactionBefore);
  assert.equal(h.runtime.session.autoCompactionEnabled, true);
  assert.equal(h.runtime.session.sessionManager.getEntries().some(entry => entry.type === 'compaction'), false);
}
const lines = async file => (await readFile(file, 'utf8')).trim().split('\n').map(line => JSON.parse(line));

test('handoff_pickup: handoff persisted before newSession, parent linked, old session intact, status on rotated endpoint', { timeout: 15000 }, async () => {
  const h = await harness();
  let release;
  try {
    const oldSession = h.runtime.session;
    const oldFile = oldSession.sessionFile, oldId = oldSession.sessionId;
    const oldBytes = await readFile(oldFile);
    const before = await h.manifest();
    // v2 manifest stays byte-compatible: no pickup capability in discovery.
    assert.equal(before.protocol_version, 2);
    assert.equal(Object.hasOwn(before.capabilities, 'handoff_pickup'), false);
    const operationId = randomUUID();
    const { socket: oldSocket, body: armBody, reply: armed } = await h.arm(operationId);
    assert.equal(armed.protocol_version, 3);
    assert.equal(armed.state, 'accepted'); assert.equal(armed.code, 'armed');
    assert.deepEqual(armed.pickup, { stage: 'handoff_running', old_session: { session_id: oldId, session_file: oldFile }, handoff: null, new_session: null, loaded_entry_id: null });
    assert.equal(armed.metadata.capabilities.handoff_pickup.protocol_version, 3);
    assert.equal(h.calls.handoff, 0, 'arming has no effect');
    // Re-arming the same request replays the record, never arms twice.
    const replay = await request(oldSocket, armBody);
    assert.equal(replay.code, 'armed'); assert.equal(globalThis[REGISTRY].ops.get(operationId).armed !== undefined, true);
    // The pickup turn blocks until released: proves the entry is loaded before the model answers.
    h.onTurn = (model, stream) => { release = () => { stream.push({ type: 'done', reason: 'stop', message: assistant(model, 'Picked up.') }); stream.end(); }; };
    await h.deliver(`/agentcenter-pickup ${operationId}`);
    assert.equal(h.calls.handoff, 1);
    assert.equal(h.spies.newSession, 1);
    // Order: at the moment newSession was requested, the handoff was already
    // persisted (complete file on disk, sha256 matching the record).
    const [seen] = h.spies.beforeNewSession;
    assert.equal(seen.code, 'new_session_requested');
    assert.equal(seen.files.length, 1);
    assert.deepEqual(seen.handoff && { path: seen.handoff.path, sha256: seen.handoff.sha256 }, seen.files[0]);
    assert.equal(seen.options.parentSession, oldFile);
    // Replacement happened in this process; the old endpoint/nonce are revoked.
    assert.notEqual(h.runtime.session, oldSession);
    const after = await h.manifest();
    assert.notEqual(after.instance_nonce, before.instance_nonce);
    assert.equal(after.pid, before.pid); assert.equal(after.process_starttime, before.process_starttime);
    await assert.rejects(request(oldSocket, { ...armBody, action: 'status' }), /ENOENT|ECONNREFUSED/);
    const { socket: newSocket, body: statusBody } = await h.body('status', operationId);
    const oldNonce = await request(newSocket, { ...statusBody, instance_nonce: before.instance_nonce });
    assert.deepEqual([oldNonce.state, oldNonce.code], ['rejected', 'unauthorized']);
    // Same operation_id, queried on the new endpoint, with evidence.
    const status = await request(newSocket, statusBody);
    assert.equal(status.protocol_version, 3);
    assert.deepEqual([status.state, status.code, status.pickup.stage], ['completed', 'pickup_loaded', 'pickup_loaded']);
    const newManager = h.runtime.session.sessionManager;
    assert.deepEqual(status.pickup.old_session, { session_id: oldId, session_file: oldFile });
    assert.deepEqual(status.pickup.new_session, { session_id: newManager.getSessionId(), session_file: newManager.getSessionFile(), parent_session: oldFile });
    assert.notEqual(status.pickup.new_session.session_id, oldId);
    const handoffBytes = await readFile(status.pickup.handoff.path);
    assert.deepEqual(status.pickup.handoff, { path: seen.files[0].path, sha256: sha256(handoffBytes), size: handoffBytes.length });
    assert.equal((await stat(status.pickup.handoff.path)).mode & 0o777, 0o600);
    // The loading entry exists before the model answered the pickup turn.
    assert.equal(typeof release, 'function', 'pickup turn reached the provider');
    const entry = newManager.getEntry(status.pickup.loaded_entry_id);
    assert.equal(entry.message.role, 'user');
    assert.equal(entry.message.content.map?.(block => block.text).join('') ?? entry.message.content, pickupMessage(status.pickup.handoff.path, status.pickup.handoff.sha256));
    // No handoff content in any reply (paths, hashes and codes only).
    assert.equal(JSON.stringify(status).includes('AGENTCENTER-CANARY-cc5'), false);
    assert.equal(JSON.stringify(armed).includes('AGENTCENTER-CANARY-cc5'), false);
    release(); await h.runtime.session.waitForIdle();
    // On disk: old session byte-identical; new header points at it; loaded entry persisted.
    assert.deepEqual(await readFile(oldFile), oldBytes);
    const persisted = await lines(newManager.getSessionFile());
    assert.equal(persisted[0].type, 'session');
    assert.equal(persisted[0].parentSession, oldFile);
    assert.equal(persisted.some(item => item.id === status.pickup.loaded_entry_id), true);
    // Double click after completion: same operation_id never creates a second session.
    await h.deliver(`/agentcenter-pickup ${operationId}`);
    assert.equal(h.spies.newSession, 1); assert.equal(h.calls.handoff, 1);
    // The arming request against the new identity is a different fingerprint.
    const rearm = await h.arm(operationId);
    assert.deepEqual([rearm.reply.state, rearm.reply.code], ['rejected', 'duplicate_mismatch']);
    // v2 clients never see the pickup record.
    const { socket, body } = await h.body('status', operationId, 2);
    assert.deepEqual([(await request(socket, body)).code], ['duplicate_mismatch']);
    assert.equal(h.spies.newSession, 1);
    assertNoReplacement(h);
    assert.deepEqual(h.errors, []);
  } finally { release?.(); await h.close(); }
});

test('handoff_pickup negatives: unarmed command, other operation, expired arm, late delivery', { timeout: 15000 }, async () => {
  const h = await harness();
  const ttl = process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS;
  try {
    const oldSession = h.runtime.session;
    // Command without any armed request (random id, garbage, empty): nothing happens.
    for (const args of [randomUUID(), 'not-a-uuid', '', `${randomUUID()} extra`]) await h.deliver(`/agentcenter-pickup ${args}`.trim());
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [0, 0]);
    // operation_id of another request (a v2 Handoff) never becomes a pickup.
    const { socket, body } = await h.body('handoff', randomUUID(), 2);
    assert.equal((await request(socket, body)).state, 'accepted');
    let handoff;
    for (let i = 0; i < 200; i++) { handoff = await request(socket, { ...body, action: 'status' }); if (TERMINAL.includes(handoff.state)) break; await sleep(5); }
    assert.equal(handoff.code, 'persisted');
    await h.deliver(`/agentcenter-pickup ${body.operation_id}`);
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [1, 0]);
    assert.deepEqual(await request(socket, { ...body, action: 'status' }), handoff);
    // Arm expires: the command after the window is refused and status says why.
    process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS = '50';
    const expired = (await h.arm()).body.operation_id;
    await sleep(80);
    await h.deliver(`/agentcenter-pickup ${expired}`);
    const expiredStatus = await h.status(expired);
    assert.deepEqual([expiredStatus.state, expiredStatus.code], ['rejected', 'arm_expired']);
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [1, 0]);
    if (ttl === undefined) delete process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS; else process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS = ttl;
    // Late delivery: A cancelled, B armed; the command for A neither runs A nor touches B.
    const a = (await h.arm()).body.operation_id;
    // Only one armed pickup per process.
    assert.deepEqual([(await h.arm()).reply.code], ['busy']);
    const cancelled = await h.status(a, 'cancel');
    assert.deepEqual([cancelled.state, cancelled.code], ['cancelled', 'cancelled']);
    const b = (await h.arm()).body.operation_id;
    await h.deliver(`/agentcenter-pickup ${a}`);
    assert.deepEqual([(await h.status(a)).code, (await h.status(b)).code], ['cancelled', 'armed']);
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [1, 0]);
    await h.status(b, 'cancel');
    assert.equal(h.runtime.session, oldSession);
    assertNoReplacement(h);
  } finally {
    if (ttl === undefined) delete process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS; else process.env.AGENTCENTER_PI_PICKUP_ARM_TTL_MS = ttl;
    await h.close();
  }
});

test('handoff_pickup negatives: busy refuses arm and command, concurrent double click runs once', { timeout: 15000 }, async () => {
  const h = await harness();
  let release;
  try {
    const oldSession = h.runtime.session;
    h.onTurn = (model, stream) => { release = () => { stream.push({ type: 'done', reason: 'stop', message: assistant(model, 'Done.') }); stream.end(); }; };
    const turn = h.deliver('Synthetic user work');
    for (let i = 0; i < 200 && !release; i++) await sleep(5);
    // A client holding the current revision (discovery may lag the running
    // turn): the refusal is busy, with no record and no effect.
    const fresh = await h.body('handoff_pickup');
    fresh.body.expected = h.runtime.session.sessionManager.getGuardedCompactionState();
    const busy = { body: fresh.body, reply: await request(fresh.socket, fresh.body) };
    assert.deepEqual([busy.reply.state, busy.reply.code], ['rejected', 'busy']);
    assert.equal(busy.reply.metadata.capabilities.handoff_pickup.disabled_reason, 'busy');
    assert.equal(globalThis[REGISTRY].ops.has(busy.body.operation_id), false);
    // A stale snapshot is refused as well, before any busy logic.
    const stale = await h.arm();
    assert.deepEqual([stale.reply.state, stale.reply.code], ['rejected', 'stale_revision']);
    assert.equal(globalThis[REGISTRY].ops.has(stale.body.operation_id), false);
    release(); await turn; await h.runtime.session.waitForIdle(); release = undefined;
    // Armed while idle, then work starts before the command arrives: refused at
    // execution. The new user turn also moves the leaf, and the revision check
    // runs before the busy check, so the refusal reports stale_revision.
    const armed = (await h.arm()).body.operation_id;
    const second = h.deliver('More synthetic work');
    for (let i = 0; i < 200 && !release; i++) await sleep(5);
    assert.equal(h.runtime.session.isStreaming, true);
    await h.deliver(`/agentcenter-pickup ${armed}`);
    const refused = await h.status(armed);
    assert.deepEqual([refused.state, refused.code, refused.pickup.handoff], ['rejected', 'stale_revision', null]);
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [0, 0]);
    release(); await second; await h.runtime.session.waitForIdle(); release = undefined;
    assert.equal(h.runtime.session, oldSession);
    // Concurrent double click on a fresh arm: exactly one handoff, one newSession.
    h.onTurn = (model, stream) => { stream.push({ type: 'done', reason: 'stop', message: assistant(model, 'Picked up.') }); stream.end(); };
    const op = (await h.arm()).body.operation_id;
    await Promise.all([h.deliver(`/agentcenter-pickup ${op}`), h.deliver(`/agentcenter-pickup ${op}`)]);
    await h.runtime.session.waitForIdle();
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [1, 1]);
    assert.equal((await h.status(op)).code, 'pickup_loaded');
    assertNoReplacement(h);
  } finally { release?.(); await h.close(); }
});

test('handoff_pickup negatives: model failure and stale revision create no session; no file on failure', { timeout: 15000 }, async () => {
  const h = await harness();
  try {
    const oldSession = h.runtime.session;
    h.onHandoff = (model, stream) => { const message = assistant(model, '', { stopReason: 'error', errorMessage: 'synthetic-provider-detail', content: [] }); stream.push({ type: 'error', reason: 'error', error: message }); stream.end(); };
    const failed = (await h.arm()).body.operation_id;
    await h.deliver(`/agentcenter-pickup ${failed}`);
    const failure = await h.status(failed);
    assert.deepEqual([failure.state, failure.code, failure.pickup.stage, failure.pickup.handoff, failure.pickup.new_session], ['failed', 'model_error', 'handoff_running', null, null]);
    assert.equal(JSON.stringify(failure).includes('synthetic-provider-detail'), false);
    assert.deepEqual((await readdir(h.handoffs)).filter(name => !name.startsWith('.')), []);
    // Revision changes while the model generates: obsolete, nothing published, no session.
    h.onHandoff = (model, stream) => {
      oldSession.sessionManager.appendMessage({ role: 'user', content: 'Concurrent synthetic entry', timestamp: Date.now() });
      stream.push({ type: 'done', reason: 'stop', message: assistant(model, HANDOFF_TEXT) }); stream.end();
    };
    const stale = (await h.arm()).body.operation_id;
    await h.deliver(`/agentcenter-pickup ${stale}`);
    const obsolete = await h.status(stale);
    assert.deepEqual([obsolete.state, obsolete.code, obsolete.pickup.new_session], ['rejected', 'stale_revision', null]);
    assert.deepEqual((await readdir(h.handoffs)).filter(name => !name.startsWith('.')), []);
    assert.deepEqual([h.calls.handoff, h.spies.newSession], [2, 0]);
    assert.equal(h.runtime.session, oldSession);
    assertNoReplacement(h);
  } finally { await h.close(); }
});

test('handoff_pickup: cancelled newSession keeps the old session active and intact', { timeout: 15000 }, async () => {
  const h = await harness({ cancelSwitch: true });
  try {
    const oldSession = h.runtime.session;
    const oldFile = oldSession.sessionFile, oldBytes = await readFile(oldFile);
    const before = await h.manifest();
    const op = (await h.arm()).body.operation_id;
    await h.deliver(`/agentcenter-pickup ${op}`);
    assert.equal(h.spies.newSession, 1);
    // session_before_switch cancelled: same session, same endpoint/nonce still serving.
    assert.equal(h.runtime.session, oldSession);
    assert.equal((await h.manifest()).instance_nonce, before.instance_nonce);
    const status = await h.status(op);
    assert.deepEqual([status.state, status.code, status.pickup.stage, status.pickup.new_session], ['failed', 'new_session_cancelled', 'handoff_persisted', null]);
    // The handoff stays persisted (the resume route), the session file untouched.
    const bytes = await readFile(status.pickup.handoff.path);
    assert.equal(sha256(bytes), status.pickup.handoff.sha256);
    assert.deepEqual(await readFile(oldFile), oldBytes);
    // The process is free again: a fresh arm is accepted (no stuck busy).
    const next = await h.arm();
    assert.equal(next.reply.code, 'armed');
    await h.status(next.body.operation_id, 'cancel');
    assertNoReplacement(h);
  } finally { await h.close(); }
});
