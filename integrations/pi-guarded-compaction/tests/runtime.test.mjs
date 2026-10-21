import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, rename, mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createAgentSession, createAgentSessionRuntime, AgentSession, SessionManager } from '../../../target/pi-guarded-runtime/dist/index.js';
import { ModelRuntime } from '../../../target/pi-guarded-runtime/dist/core/model-runtime.js';
import { AuthStorage } from '../../../target/pi-guarded-runtime/dist/core/auth-storage.js';
import { SettingsManager } from '../../../target/pi-guarded-runtime/dist/core/settings-manager.js';
import { DefaultResourceLoader } from '../../../target/pi-guarded-runtime/dist/core/resource-loader.js';
import { createAssistantMessageEventStream } from '../../../target/pi-guarded-runtime/node_modules/@earendil-works/pi-ai/dist/index.js';
// PI_SDK_DIR optionally selects the read-only upstream SDK; otherwise use the preparation receipt.
const receipt = JSON.parse(await readFile(new URL('../../../target/pi-guarded-runtime/agentcenter-ready.json', import.meta.url), 'utf8'));
const { AgentSession: GlobalSession } = await import(pathToFileURL(join(process.env.PI_SDK_DIR || receipt.source, 'dist/core/agent-session.js')).href);

globalThis.fetch = () => { throw new Error('NETWORK FORBIDDEN'); };
const usage = { input: 100, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 105, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
const tick = () => new Promise(r => setTimeout(r, 2));
async function until(predicate) { for (let i = 0; i < 500; i++) { if (predicate()) return; await tick(); } assert.fail('synthetic provider did not start'); }
async function fixture(t, { auto = false, memory = false } = {}) {
  const root = await mkdtemp(resolve(import.meta.dirname, '../../../target/cc0-test-'));
  const p = { calls: 0, deferred: false, releases: [], signals: [], hook: undefined };
  let session;
  t.after(async () => { for (const release of p.releases) release(); session?.dispose(); await rm(root, { recursive: true, force: true }); });
  const runtime = await ModelRuntime.create({ credentials: AuthStorage.inMemory(), modelsPath: null, modelsStorePath: join(root, 'models-store.json'), refreshOnCreate: false, allowModelNetwork: false });
  runtime.registerProvider('cc0-faux', { baseUrl: 'http://invalid.invalid', apiKey: 'synthetic', api: 'cc0-faux', models: [{ id: 'fake', name: 'fake', input: ['text'], reasoning: false, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 2048 }], streamSimple(model, _context, options) {
    p.calls++; p.signals.push(options?.signal);
    if (p.providerError) throw new Error('Synthetic provider failure');
    const stream = createAssistantMessageEventStream(); let done = false;
    const release = () => { if (done) return; done = true; const message = { role: 'assistant', content: [{ type: 'text', text: 'Synthetic summary.' }], api: model.api, provider: model.provider, model: model.id, stopReason: 'stop', timestamp: Date.now(), usage }; stream.push({ type: 'done', reason: 'stop', message }); stream.end(); };
    p.releases.push(release); if (!p.deferred) release(); return stream;
  } });
  const settingsManager = SettingsManager.inMemory({ compaction: { enabled: auto, keepRecentTokens: 256, reserveTokens: auto ? 65500 : 1024 }, retry: { enabled: false } });
  const loader = new DefaultResourceLoader({ cwd: root, agentDir: root, settingsManager, noExtensions: true, noSkills: true, noThemes: true, noPromptTemplates: true, noContextFiles: true, extensionFactories: [pi => {
    pi.on('session_start', (_e, ctx) => { p.ctx = ctx; });
    pi.on('session_before_compact', (e, ctx) => p.hook?.(e, ctx));
    pi.on('session_compact', (e, ctx) => p.postHook?.(e, ctx));
    pi.registerCommand('cc0-leaf', { handler: async () => { pi.appendEntry('cc0-concurrent', { synthetic: true }); } });
  }] });
  await loader.reload();
  const manager = memory ? SessionManager.inMemory(root) : SessionManager.create(root, join(root, 'sessions'));
  for (let i = 0; i < 12; i++) {
    manager.appendMessage({ role: 'user', content: `Synthetic ${i} ` + 'history '.repeat(500), timestamp: Date.now() });
    manager.appendMessage({ role: 'assistant', content: [{ type: 'text', text: 'Synthetic response.' }], api: 'cc0-faux', provider: 'cc0-faux', model: 'fake', stopReason: 'stop', timestamp: Date.now(), usage });
  }
  ({ session } = await createAgentSession({ cwd: root, agentDir: root, modelRuntime: runtime, model: runtime.getModel('cc0-faux', 'fake'), sessionManager: manager, settingsManager, resourceLoader: loader, noTools: 'all' }));
  await session.bindExtensions({ onError: error => { p.extensionError = error; } });
  return { p, session, manager, root, runtime, settingsManager, loader, expected: () => manager.getGuardedCompactionState(), entries: async () => (await readFile(manager.getSessionFile(), 'utf8')).trim().split('\n').map(JSON.parse) };
}
const noCompaction = f => assert.equal(f.manager.getEntries().filter(e => e.type === 'compaction').length, 0);

// Regression: failed atomic rename must leave every in-memory identity/context unchanged.
test('EISDIR is sanitized failed with no entry leaf revision or context mutation', async t => {
  const f = await fixture(t); const before = f.expected(); const entries = structuredClone(f.manager.getEntries());
  const messages = structuredClone(f.session.messages); const file = f.manager.getSessionFile();
  await rename(file, file + '.backup'); await mkdir(file);
  await assert.rejects(f.session.compactGuarded({ expected: before }), { code: 'failed', message: 'failed' });
  assert.deepEqual(f.expected(), before); assert.deepEqual(f.manager.getEntries(), entries);
  assert.deepEqual(f.session.messages, messages); noCompaction(f);
});

test('inMemory rejects non_persistent before provider or hooks', async t => {
  const f = await fixture(t, { memory: true }); const before = f.expected(); let hooks = 0;
  f.p.hook = () => { hooks++; };
  await assert.rejects(f.p.ctx.compactGuarded({ expected: before }), { code: 'non_persistent' });
  assert.equal(hooks, 0); assert.equal(f.p.calls, 0); assert.deepEqual(f.expected(), before); noCompaction(f);
});

for (const timing of ['initial', 'provider', 'hook']) {
  test(`public nextTurn queue at ${timing} rejects pending without aborting provider`, async t => {
    const f = await fixture(t); const queue = () => f.session.sendCustomMessage({ customType: 'synthetic', content: 'Synthetic pending', display: false }, { deliverAs: 'nextTurn' });
    if (timing === 'initial') await queue();
    if (timing === 'hook') f.p.hook = queue;
    f.p.deferred = timing === 'provider';
    const operation = f.session.compactGuarded({ expected: f.expected() });
    const rejected = assert.rejects(operation, { code: 'pending' });
    if (timing === 'provider') { await until(() => f.p.calls === 1); await queue(); assert.equal(f.p.signals[0].aborted, false); f.p.releases[0](); }
    await rejected; noCompaction(f); if (timing !== 'provider') assert.equal(f.p.calls, 0);
  });
}

test('public followUp queued during provider rejects pending without abort', async t => {
  const f = await fixture(t); f.p.deferred = true;
  const rejected = assert.rejects(f.session.compactGuarded({ expected: f.expected() }), { code: 'pending' });
  await until(() => f.p.calls === 1); await f.session.followUp('Synthetic pending');
  assert.equal(f.p.signals[0].aborted, false); f.p.releases[0](); await rejected; noCompaction(f);
});

for (const kind of ['cancelled', 'stale']) {
  test(`details.toJSON ${kind} at final serialization prevents atomic commit`, async t => {
    const f = await fixture(t); const controller = new AbortController(); let calls = 0;
    f.p.hook = e => ({ compaction: { summary: 'Synthetic extension summary', firstKeptEntryId: e.preparation.firstKeptEntryId, tokensBefore: e.preparation.tokensBefore,
      details: { toJSON() { calls++; if (kind === 'cancelled') controller.abort(); else f.manager.branch(f.manager.getEntries()[0].id); return { synthetic: true }; } } } });
    await assert.rejects(f.session.compactGuarded({ expected: f.expected(), signal: controller.signal }), { code: kind });
    assert.equal(calls, 1); noCompaction(f); assert.equal((await f.entries()).some(e => e.type === 'compaction'), false);
  });
}

test('provider failure emits one start and one sanitized terminal event', async t => {
  const f = await fixture(t); const events = []; f.session.subscribe(e => events.push(e)); f.p.providerError = true;
  await assert.rejects(f.session.compactGuarded({ expected: f.expected() }), { code: 'failed' });
  assert.equal(events.filter(e => e.type === 'compaction_start').length, 1);
  const terminal = events.filter(e => e.type === 'compaction_end'); assert.equal(terminal.length, 1);
  assert.equal(terminal[0].errorMessage, 'failed'); assert.equal(terminal[0].result, undefined);
  assert.doesNotMatch(JSON.stringify(terminal), /Synthetic provider failure|Synthetic summary|history /);
});

test('postcommit manager session switch preserves original metadata and context', async t => {
  const f = await fixture(t); const before = f.expected(); let next;
  f.p.postHook = async () => { f.manager.newSession(); next = structuredClone(f.session.messages); };
  const result = await f.session.compactGuarded({ expected: before });
  assert.equal(result.sessionId, before.sessionId); assert.equal(result.sessionFile, before.sessionFile);
  assert.equal(result.previousLeafId, before.leafId); assert.notEqual(f.manager.getSessionId(), before.sessionId);
  assert.deepEqual(f.session.messages, next);
  const disk = (await readFile(before.sessionFile, 'utf8')).trim().split('\n').map(JSON.parse);
  assert.equal(disk.find(e => e.id === result.entryId).type, 'compaction');
});

test('public ctx capability completes with persisted metadata, global SDK remains unpatched', async t => {
  const f = await fixture(t); assert.equal(typeof AgentSession.prototype.compactGuarded, 'function'); assert.equal(GlobalSession.prototype.compactGuarded, undefined);
  assert.equal(f.p.ctx.compactGuardedCapability, 'agentcenter.compactGuarded.v1');
  assert.equal(f.session.compactGuardedCapability, 'agentcenter.compactGuarded.v1');
  assert.equal(typeof f.p.ctx.compactGuarded, 'function'); const expected = f.p.ctx.sessionManager.getGuardedCompactionState();
  const result = await f.p.ctx.compactGuarded({ expected });
  assert.equal(result.capability, 'agentcenter.compactGuarded.v1'); assert.equal(result.previousLeafId, expected.leafId);
  const entry = (await f.entries()).find(e => e.id === result.entryId); assert.equal(entry.type, 'compaction'); assert.equal(entry.parentId, expected.leafId); assert.equal(f.manager.getLeafId(), entry.id);
  assert.equal('summary' in result, false); assert.equal(f.p.calls, 1);
});

test('public appendEntry during deferred provider rejects stale without context switch or compaction', async t => {
  const f = await fixture(t); f.p.deferred = true;
  const operation = f.session.compactGuarded({ expected: f.expected() }); const rejected = assert.rejects(operation, { code: 'stale' });
  await until(() => f.p.calls === 1); await f.session.prompt('/cc0-leaf'); const leaf = f.manager.getLeafId(); const messages = f.session.messages;
  f.p.releases[0](); await rejected; noCompaction(f); assert.equal(f.manager.getLeafId(), leaf); assert.deepEqual(f.session.messages, messages);
  assert.equal((await f.entries()).some(e => e.type === 'compaction'), false);
});

test('ABA leaf return during generation is stale because revision is monotonic', async t => {
  const f = await fixture(t); const expected = f.expected(); f.p.deferred = true;
  const operation = f.session.compactGuarded({ expected }); const rejected = assert.rejects(operation, { code: 'stale' });
  await until(() => f.p.calls === 1); f.manager.branch(f.manager.getEntries()[0].id); f.manager.branch(expected.leafId);
  assert.equal(f.manager.getLeafId(), expected.leafId); assert.notEqual(f.expected().revision, expected.revision);
  f.p.releases[0](); await rejected; noCompaction(f);
});

test('wrong manager/session/file/revision/leaf and pre-aborted signal cause no provider effects', async t => {
  const f = await fixture(t); const expected = f.expected();
  for (const field of ['managerId', 'sessionId', 'sessionFile', 'revision', 'leafId']) {
    await assert.rejects(f.session.compactGuarded({ expected: { ...expected, [field]: field === 'revision' ? '999999' : 'wrong' } }), { code: 'stale' });
  }
  const controller = new AbortController(); controller.abort();
  await assert.rejects(f.session.compactGuarded({ expected, signal: controller.signal }), { code: 'cancelled' });
  assert.equal(f.p.calls, 0); assert.deepEqual(f.expected(), expected); noCompaction(f);
});

test('second reservation rejects busy, its cancellation cannot abort the first owner', async t => {
  const f = await fixture(t); f.p.deferred = true; const first = f.session.compactGuarded({ expected: f.expected() });
  await until(() => f.p.calls === 1); const other = new AbortController();
  await assert.rejects(f.session.compactGuarded({ expected: f.expected(), signal: other.signal }), { code: 'busy' }); other.abort();
  assert.equal(f.p.signals[0].aborted, false); f.p.releases[0](); const result = await first; assert.equal(f.manager.getEntry(result.entryId).type, 'compaction');
});

test('owner cancellation during generation prevents persistence and releases reservation', async t => {
  const f = await fixture(t); f.p.deferred = true; const controller = new AbortController();
  const operation = f.session.compactGuarded({ expected: f.expected(), signal: controller.signal }); const rejected = assert.rejects(operation, { code: 'cancelled' });
  await until(() => f.p.calls === 1); controller.abort(); f.p.releases[0](); await rejected; noCompaction(f);
  f.p.deferred = false; const result = await f.session.compactGuarded({ expected: f.expected() }); assert.equal(f.manager.getEntry(result.entryId).type, 'compaction');
});

test('active foreign turn and queued messages reject without aborting its provider', async t => {
  const f = await fixture(t); f.p.deferred = true; const turn = f.session.prompt('Synthetic active turn'); await until(() => f.p.calls === 1);
  await assert.rejects(f.session.compactGuarded({ expected: f.expected() }), { code: 'busy' }); assert.equal(f.p.signals[0].aborted, false);
  await f.session.followUp('Synthetic queued turn');
  await assert.rejects(f.session.compactGuarded({ expected: f.expected() }), { code: 'pending' }); assert.equal(f.p.signals[0].aborted, false);
  f.session.clearQueue(); f.p.deferred = false; f.p.releases[0](); await turn; noCompaction(f);
});

test('cancelled hook releases reservation and deferred hook mutation is revalidated', async t => {
  const f = await fixture(t); f.p.hook = () => ({ cancel: true });
  await assert.rejects(f.session.compactGuarded({ expected: f.expected() }), { code: 'cancelled' }); noCompaction(f); assert.equal(f.p.calls, 0);
  f.p.hook = async () => { await Promise.resolve(); await f.session.prompt('/cc0-leaf'); };
  await assert.rejects(f.session.compactGuarded({ expected: f.expected() }), { code: 'stale' }); noCompaction(f); assert.equal(f.p.calls, 0);
  f.p.hook = undefined; assert.ok((await f.session.compactGuarded({ expected: f.expected() })).entryId);
});

test('provider error rejects without persistence and a later operation can reserve again', async t => {
  const f = await fixture(t); f.p.providerError = true;
  await assert.rejects(f.session.compactGuarded({ expected: f.expected() }), error => {
    assert.equal(error.code, 'failed'); assert.equal(error.message, 'failed');
    assert.equal(error.cause, undefined);
    assert.doesNotMatch(JSON.stringify(error) + error.stack, /Synthetic provider failure|Synthetic summary|history /);
    return true;
  });
  noCompaction(f); f.p.providerError = false;
  const result = await f.session.compactGuarded({ expected: f.expected() });
  assert.equal(f.manager.getEntry(result.entryId).type, 'compaction');
});

test('legacy control still persists on concurrent public append leaf (absence of CAS, not context loss)', async t => {
  const f = await fixture(t); f.p.deferred = true; const operation = f.session.compact();
  await until(() => f.p.calls === 1); await f.session.prompt('/cc0-leaf'); const leaf = f.manager.getLeafId(); f.p.releases[0](); await operation;
  const entry = (await f.entries()).findLast(e => e.type === 'compaction'); assert.ok(entry); assert.equal(entry.parentId, leaf);
});

// Regression: a reload must invalidate both the captured ctx and an in-flight summary.
test('reload during deferred provider invalidates old context and rejects late persistence', async t => {
  const f = await fixture(t); f.p.deferred = true; const old = f.p.ctx;
  const operation = old.compactGuarded({ expected: f.expected() });
  const rejected = assert.rejects(operation, { code: 'stale' });
  await until(() => f.p.calls === 1); await f.session.reload();
  assert.notEqual(f.p.ctx, old); assert.throws(() => old.compactGuarded, /stale|invalid/i);
  const messages = f.session.messages; f.p.releases[0](); await rejected;
  noCompaction(f); assert.deepEqual(f.session.messages, messages);
  assert.equal((await f.entries()).some(e => e.type === 'compaction'), false);
  f.p.deferred = false;
  assert.ok((await f.p.ctx.compactGuarded({ expected: f.expected() })).entryId);
});

// Regression: equal on-disk identity must not authorize a different manager instance.
test('public runtime replacement with same file/session/leaf rejects old identity and late operation', async t => {
  const f = await fixture(t); let first = true;
  const factory = async options => {
    const result = first ? (first = false, { session: f.session, extensionsResult: f.loader.getExtensions() }) : await createAgentSession({ ...options, modelRuntime: f.runtime, model: f.runtime.getModel('cc0-faux', 'fake'), settingsManager: f.settingsManager, resourceLoader: f.loader, noTools: 'all' });
    return { ...result, services: { cwd: f.root, agentDir: f.root }, diagnostics: [] };
  };
  const owner = await createAgentSessionRuntime(factory, { cwd: f.root, agentDir: f.root, sessionManager: f.manager });
  owner.setRebindSession(async session => session.bindExtensions({ onError: () => {} }));
  t.after(() => owner.dispose()); const expected = f.expected(); f.p.deferred = true;
  const operation = owner.session.compactGuarded({ expected }); const rejected = assert.rejects(operation, /stale|cancel/i);
  await until(() => f.p.calls === 1);
  const switching = owner.switchSession(f.manager.getSessionFile());
  await until(() => f.p.signals[0].aborted); f.p.releases[0](); await switching;
  const next = owner.session; const state = next.sessionManager.getGuardedCompactionState();
  for (const field of ['sessionId', 'sessionFile', 'leafId']) assert.equal(state[field], expected[field]);
  assert.notEqual(state.managerId, expected.managerId);
  await assert.rejects(next.compactGuarded({ expected }), { code: 'stale' });
  const messages = next.messages; f.p.releases[0](); await rejected;
  assert.deepEqual(next.messages, messages); noCompaction(f);
  assert.equal(next.sessionManager.getEntries().some(e => e.type === 'compaction'), false);
  assert.equal((await f.entries()).some(e => e.type === 'compaction'), false);
});

// Regression: the legacy automatic path still runs from prompt, not a direct compact call.
test('prompt triggers enabled automatic threshold compaction and preserves usable context', async t => {
  const f = await fixture(t, { auto: true }); const events = [];
  f.session.subscribe(e => events.push(e)); await f.session.prompt('Synthetic automatic compaction turn');
  assert.ok(events.some(e => e.type === 'auto_compaction_end' || (e.type === 'compaction_end' && e.reason === 'threshold')));
  assert.ok(f.manager.getEntries().some(e => e.type === 'compaction'));
  assert.ok(f.session.messages.length > 0); assert.ok(f.p.calls >= 2);
  assert.equal(f.session.isStreaming, false);
});

// Regression: postcommit replacement must not apply the old context to the new runtime.
test('postcommit public runtime switch preserves committed metadata and new session', async t => {
  const f = await fixture(t); const target = await fixture(t); let first = true;
  const factory = async options => {
    const result = first ? (first = false, { session: f.session, extensionsResult: f.loader.getExtensions() }) : await createAgentSession({ ...options, modelRuntime: f.runtime, model: f.runtime.getModel('cc0-faux', 'fake'), settingsManager: f.settingsManager, resourceLoader: f.loader, noTools: 'all' });
    return { ...result, services: { cwd: f.root, agentDir: f.root }, diagnostics: [] };
  };
  const owner = await createAgentSessionRuntime(factory, { cwd: f.root, agentDir: f.root, sessionManager: f.manager });
  owner.setRebindSession(async session => session.bindExtensions({ onError: () => {} }));
  t.after(() => owner.dispose()); const before = f.expected();
  const targetFile = target.manager.getSessionFile(); const diskBefore = await readFile(targetFile, 'utf8');
  let next, state, messages;
  f.p.postHook = async () => {
    assert.equal((await owner.switchSession(targetFile)).cancelled, false);
    next = owner.session; state = next.sessionManager.getGuardedCompactionState(); messages = structuredClone(next.messages);
  };
  const result = await owner.session.compactGuarded({ expected: before });
  assert.ok(next); assert.notEqual(next, f.session); assert.equal(owner.session, next);
  assert.equal(result.sessionId, before.sessionId); assert.equal(result.sessionFile, before.sessionFile);
  assert.equal(result.previousLeafId, before.leafId);
  assert.equal(state.sessionId, target.manager.getSessionId()); assert.equal(state.sessionFile, targetFile);
  assert.deepEqual(next.sessionManager.getGuardedCompactionState(), state); assert.deepEqual(next.messages, messages);
  assert.equal(await readFile(targetFile, 'utf8'), diskBefore);
  assert.equal(next.sessionManager.getEntries().some(e => e.type === 'compaction'), false);
  const committed = (await f.entries()).filter(e => e.type === 'compaction');
  assert.equal(committed.length, 1); assert.equal(committed[0].id, result.entryId);
});

// Regression: final hook cancellation must not commit or keep ownership of a later operation.
test('final hook cancellation, hook throw, and old signal cannot cancel a new owner', async t => {
  const f = await fixture(t); const old = new AbortController();
  f.p.hook = e => { old.abort(); return { compaction: { summary: 'Synthetic hook summary', firstKeptEntryId: e.preparation.firstKeptEntryId, tokensBefore: e.preparation.tokensBefore } }; };
  await assert.rejects(f.session.compactGuarded({ expected: f.expected(), signal: old.signal }), { code: 'cancelled' }); noCompaction(f);
  f.p.hook = () => { throw new Error('Synthetic hook failure'); };
  // Extension errors are routed through the public error callback, not necessarily propagated.
  const initialLeaf = f.manager.getLeafId(); const finished = new AbortController();
  await f.session.compactGuarded({ expected: f.expected(), signal: finished.signal });
  assert.ok(f.p.extensionError); assert.match(String(f.p.extensionError.error), /Synthetic hook failure/);
  f.manager.branch(initialLeaf); f.p.hook = undefined; f.p.deferred = true;
  const controller = new AbortController(); const expected = f.expected(); const before = f.p.calls;
  const next = f.session.compactGuarded({ expected, signal: controller.signal });
  await until(() => f.p.calls > before); finished.abort(); assert.equal(f.p.signals.at(-1).aborted, false);
  f.p.releases.at(-1)(); assert.ok((await next).entryId);
});
