import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, readdir, lstat, rm } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { createOperations } from './operations.mjs';
const P = process.env.PI_SDK_DIR;
assert.ok(P, 'Set PI_SDK_DIR to the original read-only Pi SDK');
const { ModelRuntime } = await import(`${P}/dist/core/model-runtime.js`);
const { ModelRegistry } = await import(`${P}/dist/core/model-registry.js`);
const { AuthStorage } = await import(`${P}/dist/core/auth-storage.js`);
const { createAssistantMessageEventStream } = await import(`${P}/node_modules/@earendil-works/pi-ai/dist/index.js`);
globalThis.fetch = () => { throw new Error('NETWORK FORBIDDEN'); };

async function fixture(sessionId = 'synthetic') {
  const root = await mkdtemp(`/run/user/${process.getuid()}/ac-op-`);
  let leaf = 'leaf', busy = false, pending = false, calls = 0, finish;
  const runtime = await ModelRuntime.create({ credentials: AuthStorage.inMemory(), modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
  runtime.registerProvider('cc1-fake', { baseUrl: 'http://invalid.invalid', apiKey: 'synthetic', api: 'cc1-fake', models: [{ id: 'fake', name: 'fake', input: ['text'], reasoning: false, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 2048 }], streamSimple(model, context, options) {
    calls++;
    assert.equal(options.maxTokens, 2048);
    assert(options.signal instanceof AbortSignal);
    assert.equal(context.messages.length, 1);
    assert(Buffer.byteLength(context.messages[0].content) < 64 * 1024);
    const stream = createAssistantMessageEventStream();
    finish = (overrides = {}) => { const message = { role: 'assistant', content: [{ type: 'text', text: 'Objective: continue work. Next step: test.' }], api: model.api, provider: model.provider, model: model.id, stopReason: 'stop', timestamp: Date.now(), usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } }, ...overrides }; stream.push({ type: message.stopReason === 'error' ? 'error' : 'done', reason: message.stopReason, message, error: message }); stream.end(); };
    return stream;
  } });
  const registry = new ModelRegistry(runtime);
  const ctx = { model: registry.find('cc1-fake', 'fake'), modelRegistry: registry,
    compactGuardedCapability: 'agentcenter.compactGuarded.v1',
    compactGuarded: async ({ expected, signal }) => { assert(!signal.aborted); assert.equal(expected.leafId, leaf); return { capability: 'agentcenter.compactGuarded.v1', sessionId: 'synthetic', sessionFile: '/synthetic', previousLeafId: leaf, entryId: 'compacted' }; },
    isIdle: () => !busy, hasPendingMessages: () => pending,
    sessionManager: { getGuardedCompactionState: () => ({ managerId: 'manager', revision: leaf === 'leaf' ? '1' : '2', sessionId, sessionFile: '/synthetic', leafId: leaf }), getSessionId: () => sessionId, getSessionFile: () => '/synthetic', getLeafId: () => leaf,
      buildSessionProjection: () => ({ messages: [{ role: 'user', content: 'Work request' }] }) } };
  const manager = await createOperations({ getContext: () => ctx, outputRoot: root });
  const { capabilities, handoff_directory, model_auth, ...identity } = manager.metadata();
  const request = { protocol_version: 2, operation_id: randomUUID(), action: 'handoff', ...identity };
  return { ctx, manager, request, root, calls: () => calls, finish: overrides => finish(overrides), stale: () => { leaf = 'changed'; }, busy: () => { busy = true; }, pending: () => { pending = true; }, async close() { await manager.close(); await rm(root, { recursive: true, force: true }); } };
}
test('SDK provider quota error returns only sanitized code and no artifact', async () => {
  const f = await fixture();
  try {
    f.manager.dispatch(f.request); await tick();
    f.finish({ stopReason: 'error', errorMessage: '429 quota synthetic-private-provider-detail', content: [] });
    const result = await terminal(f);
    assert.equal(result.state, 'failed');
    assert.equal(result.code, 'model_error');
    assert.equal(JSON.stringify(result).includes('synthetic-private-provider-detail'), false);
    assert.equal(result.artifact_path, undefined);
    assert.deepEqual(await readdir(f.root), []);
    assert.equal(f.calls(), 1);
  } finally { await f.close(); }
});

test('deadline aborts SDK provider wait and late delivery cannot publish', async t => {
  const f = await fixture();
  try {
    t.mock.timers.enable({ apis: ['setTimeout'] });
    f.manager.dispatch(f.request);
    for (let i = 0; i < 1000 && f.calls() === 0; i++) await new Promise(resolve => setImmediate(resolve));
    assert.equal(f.calls(), 1);
    t.mock.timers.tick(60_000);
    t.mock.timers.reset();
    const result = await terminal(f);
    assert.equal(result.state, 'cancelled');
    assert.equal(result.code, 'timeout');
    assert.equal(result.artifact_path, undefined);
    f.finish(); await tick();
    assert.deepEqual(await terminal(f), result);
    assert.deepEqual(await readdir(f.root), []);
  } finally { t.mock.timers.reset(); await f.close(); }
});

test('unit retention tombstones expire without restarting an operation', async t => {
  const f = await fixture();
  try {
    f.manager.dispatch(f.request); await tick(); f.finish();
    assert.equal((await terminal(f)).state, 'completed');
    t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
    t.mock.timers.tick(30 * 60 * 1000 + 1);
    assert.equal(f.manager.dispatch({ ...f.request, action: 'status' }).code, 'expired');
    assert.equal(f.manager.dispatch(f.request).code, 'expired');
    assert.equal(f.calls(), 1);
  } finally { t.mock.timers.reset(); await f.close(); }
});

test('128 retained tombstones reject a 129th ID without eviction or a second effect', async t => {
  const f = await fixture();
  let effects = 0;
  const compact = f.ctx.compactGuarded;
  f.ctx.compactGuarded = async options => { effects++; return compact(options); };
  try {
    const receipts = [];
    for (let i = 0; i < 128; i++) {
      const request = { ...f.request, action: 'compact', operation_id: randomUUID() };
      assert.equal(f.manager.dispatch(request).state, 'accepted');
      const result = await terminal({ ...f, request });
      assert.equal(result.state, 'completed');
      receipts.push({ request, result });
    }
    assert.equal(effects, 128);
    const overflow = { ...f.request, action: 'compact', operation_id: randomUUID() };
    assert.equal(f.manager.dispatch(overflow).code, 'capacity');
    assert.equal(f.manager.dispatch({ ...overflow, action: 'status' }).code, 'unknown_operation');
    for (const { request, result } of receipts) assert.deepEqual(f.manager.dispatch(request), result);
    t.mock.timers.enable({ apis: ['Date'], now: Date.now() });
    t.mock.timers.tick(30 * 60 * 1000 + 1);
    for (const { request } of receipts) {
      assert.equal(f.manager.dispatch(request).code, 'expired');
      assert.equal(f.manager.dispatch({ ...request, action: 'status' }).code, 'expired');
    }
    assert.equal(f.manager.dispatch(overflow).code, 'capacity');
    assert.equal(effects, 128);
    assert.equal(f.calls(), 0);
    assert.deepEqual(await readdir(f.root), []);
  } finally { t.mock.timers.reset(); await f.close(); }
});

test('concurrent shutdown aborts generation once; late provider delivery cannot publish', { timeout: 1000 }, async () => {
  const f = await fixture();
  let signal, aborts = 0;
  const stream = f.ctx.modelRegistry.streamSimple.bind(f.ctx.modelRegistry);
  f.ctx.modelRegistry.streamSimple = (model, context, options) => {
    signal = options.signal;
    signal.addEventListener('abort', () => { aborts++; }, { once: true });
    return stream(model, context, options);
  };
  f.ctx.abort = () => assert.fail('global abort forbidden');
  try {
    f.manager.dispatch(f.request);
    for (let i = 0; i < 1000 && f.calls() === 0; i++) await new Promise(resolve => setImmediate(resolve));
    assert.equal(f.calls(), 1);
    await Promise.all([f.manager.close(), f.manager.close(), f.manager.close()]);
    assert.equal(signal.aborted, true);
    assert.equal(signal.reason, 'closed');
    assert.equal(aborts, 1);
    const result = await terminal(f);
    assert.equal(result.state, 'cancelled');
    assert.equal(result.code, 'cancelled');
    assert.equal(f.manager.dispatch({ ...f.request, operation_id: randomUUID() }).code, 'stale_instance');
    f.finish(); await tick();
    assert.deepEqual(await terminal(f), result);
    assert.equal(f.calls(), 1);
    assert.deepEqual(await readdir(f.root), []);
  } finally { await f.close(); }
});

test('overlong exact session marker refuses without model call or completed', async () => {
  const f = await fixture('é'.repeat(100));
  try {
    f.manager.dispatch(f.request);
    const done = await terminal(f);
    assert.equal(done.state, 'rejected');
    assert.equal(f.calls(), 0);
  } finally { await f.close(); }
});

test('both capabilities require selected model and configured public auth', async () => {
  const f = await fixture();
  try {
    f.ctx.model = undefined;
    for (const action of ['handoff', 'compact']) {
      assert.equal(f.manager.metadata().capabilities[action].enabled, false);
      assert.equal(f.manager.metadata().capabilities[action].disabled_reason, 'no_model');
      assert.equal(f.manager.dispatch({ ...f.request, action }).code, 'disabled');
    }
    assert.equal(f.calls(), 0);
  } finally { await f.close(); }
});

test('guarded compact preserves confirmed expected and signal, replays completion; no legacy fallback', async () => {
  const f = await fixture();
  try {
    f.request.action = 'compact';
    assert.equal(f.manager.metadata().capabilities.compact.enabled, true);
    assert.equal(f.manager.dispatch(f.request).state, 'accepted');
    const done = await terminal(f);
    assert.equal(done.state, 'completed');
    assert.equal(done.compaction.entryId, 'compacted');
    assert.equal(f.calls(), 0);
    assert.deepEqual(f.manager.dispatch(f.request), done);
  } finally { await f.close(); }
  const g = await fixture();
  try {
    g.ctx.compactGuardedCapability = 'unsupported';
    g.ctx.compact = () => assert.fail('legacy compact forbidden');
    g.ctx.abort = () => assert.fail('global abort forbidden');
    assert.equal(g.manager.metadata().capabilities.compact.enabled, false);
    assert.equal(g.manager.dispatch({ ...g.request, action: 'compact' }).code, 'unsupported_guarded_compaction');
  } finally { await g.close(); }
});
async function tick() { await new Promise(resolve => setTimeout(resolve, 5)); }
async function terminal(f) {
  for (let i = 0; i < 100; i++) { const result = f.manager.dispatch({ ...f.request, action: 'status' }); if (!['running', 'accepted'].includes(result.state)) return result; await tick(); }
  throw new Error('operation did not finish');
}

test('SDK fake provider generates real private artifact exactly once; duplicate and unknown queries', async () => {
  const f = await fixture();
  try {
    assert.equal(f.manager.dispatch(f.request).state, 'accepted');
    await tick();
    assert.equal(f.manager.dispatch(f.request).state, 'running');
    assert.equal(f.calls(), 1);
    assert.equal(f.manager.dispatch({ ...f.request, operation_id: randomUUID() }).code, 'busy');
    assert.equal(f.manager.dispatch({ ...f.request, action: 'compact' }).code, 'duplicate_mismatch');
    assert.equal(f.manager.dispatch({ ...f.request, action: 'status', operation_id: randomUUID() }).state, 'unknown');
    f.finish();
    const done = await terminal(f);
    assert.equal(done.state, 'completed');
    assert.match(done.artifact_path.split('/').at(-1), /^s73796e746865746963-[0-9a-f]{32}-[0-9a-f-]{36}\.md$/);
    assert.equal(done.cost_usd, 0);
    assert.equal(new Date(done.completed_at).toISOString(), done.completed_at);
    assert.equal(JSON.parse((await readFile(done.artifact_path, 'utf8')).split('\n')[0]).git.state, 'unavailable');
    assert.match(await readFile(done.artifact_path, 'utf8'), /Next step: test/);
    assert.equal((await lstat(done.artifact_path)).mode & 0o777, 0o600);
    assert.deepEqual(f.manager.dispatch(f.request), done);
    assert.equal(f.calls(), 1);
  } finally { await f.close(); }
});
test('stale revision after generation discards artifact; metadata refresh keeps nonce', async () => {
  const f = await fixture();
  try { f.manager.dispatch(f.request); await tick(); f.stale(); f.finish(); assert.equal((await terminal(f)).code, 'stale_revision'); assert.equal(f.manager.metadata().instance_nonce, f.request.instance_nonce); }
  finally { await f.close(); }
});
test('cancel aborts only generation and never creates completed; busy/pending refuse dispatch', async () => {
  const f = await fixture();
  try { f.manager.dispatch(f.request); await tick(); f.manager.dispatch({ ...f.request, action: 'cancel' }); assert.equal((await terminal(f)).state, 'cancelled'); f.finish(); await tick(); assert.equal((await terminal(f)).state, 'cancelled'); }
  finally { await f.close(); }
  for (const condition of ['busy', 'pending']) { const g = await fixture(); try { g[condition](); assert.equal(g.manager.dispatch(g.request).code, 'busy'); assert.equal(g.calls(), 0); } finally { await g.close(); } }
});
test('well-formed wrong nonce is unauthorized for every action without model call or record', async () => {
  const f = await fixture();
  try {
    const real = f.request.instance_nonce;
    const wrong = real.slice(0, -1) + (real.at(-1) === '0' ? '1' : '0');
    assert.match(wrong, /^[0-9a-f]{64}$/);
    assert.notEqual(wrong, real);
    for (const action of ['handoff', 'compact', 'status', 'cancel']) {
      const result = f.manager.dispatch({ ...f.request, action, instance_nonce: wrong });
      assert.deepEqual([result.state, result.code], ['rejected', 'unauthorized'], action);
    }
    await tick();
    assert.equal(f.calls(), 0);
    assert.deepEqual(await readdir(f.root), []);
    assert.equal(f.manager.dispatch({ ...f.request, action: 'status' }).code, 'unknown_operation');
  } finally { await f.close(); }
});
