import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createConnection } from 'node:net';
import { randomUUID } from 'node:crypto';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const execute = promisify(execFile);
const P = process.env.PI_SDK_DIR;
assert.ok(P, 'Set PI_SDK_DIR to the original read-only Pi SDK');
// Test-only prepared runtime; production loading remains explicit opt-in.
const PRIVATE = process.env.AGENTCENTER_GUARDED_PI_SDK;
assert.ok(PRIVATE, 'Set AGENTCENTER_GUARDED_PI_SDK to a prepared private runtime');
async function sdk(path) {
  return Object.assign({}, ...await Promise.all(['sdk', 'model-runtime', 'auth-storage', 'session-manager', 'settings-manager', 'resource-loader'].map(name => import(`${path}/dist/core/${name}.js`))));
}
const { createAgentSession, ModelRuntime, AuthStorage, SessionManager, SettingsManager, DefaultResourceLoader } = await sdk(P);
const guardedSdk = await sdk(PRIVATE).catch(error => { throw new Error(`Required private CC0 SDK missing or invalid at ${PRIVATE}; prepare it separately, no fallback`, { cause: error }); });
async function rustProbe(runtimeDir, action) {
  const cwd = resolve(import.meta.dirname, '../..');
  const cargo = join(process.env.HOME, '.cargo/bin/cargo');
  await execute(cargo, ['build', '--offline', '-p', 'agentcenter-core', '--example', 'pi_control_probe'], { cwd, timeout: 30000 });
  const operationId = randomUUID();
  const { stdout } = await execute(join(cwd, 'target/debug/examples/pi_control_probe'), [runtimeDir, action, operationId], { cwd, timeout: 12000 });
  const result = JSON.parse(stdout);
  assert.equal(result.operation_id, operationId);
  assert.equal(result.accepted, true);
  assert.ok(result.polls > 0, 'Rust polls status with the original operation UUID');
  assert.equal(result.state, 'completed');
  assert.equal(stdout.includes('instance_nonce'), false);
  assert.equal(stdout.includes('Verified handoff'), false);
  return result;
}
const { createAssistantMessageEventStream } = await import(`${P}/node_modules/@earendil-works/pi-ai/dist/index.js`);
globalThis.fetch = () => { throw new Error('NETWORK FORBIDDEN'); };
function request(socket, body) { return new Promise((resolve, reject) => { const client = createConnection(socket); let text = ''; client.on('connect', () => client.end(JSON.stringify(body) + '\n')); client.on('data', chunk => { text += chunk; }); client.on('end', () => { try { resolve(JSON.parse(text)); } catch (error) { reject(error); } }); client.on('error', reject); }); }
for (const [name, implementation, supported, crossLanguage, reloadInHook] of [['private CC0', guardedSdk, true, false], ['original SDK (no fallback)', await sdk(P), false, false], ['Rust client -> private CC0', guardedSdk, true, true], ['reload inside native compact hook', guardedSdk, true, false, true]]) {
const { createAgentSession, ModelRuntime, AuthStorage, SessionManager, SettingsManager, DefaultResourceLoader } = implementation;
test(`opt-in ${name}: socket Handoff and Compact persistence or explicit refusal`, { timeout: 10000 }, async () => {
  const root = await mkdtemp(`/run/user/${process.getuid()}/ac-sdk-`);
  const previous = { runtime: process.env.XDG_RUNTIME_DIR, state: process.env.XDG_STATE_HOME };
  let session, calls = 0, hookReload, hookEntered = false, hookCompleted = false;
  try {
    const runtimeDir = join(root, 'runtime'); await mkdir(runtimeDir, { mode: 0o700 });
    process.env.XDG_RUNTIME_DIR = runtimeDir; process.env.XDG_STATE_HOME = root;
    const runtime = await ModelRuntime.create({ credentials: AuthStorage.inMemory(), modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
    runtime.registerProvider('cc1-fake', { baseUrl: 'http://invalid.invalid', apiKey: 'synthetic', api: 'cc1-fake', models: [{ id: 'fake', name: 'fake', input: ['text'], reasoning: false, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 2048 }], streamSimple(model) {
      calls++; const stream = createAssistantMessageEventStream(); const message = { role: 'assistant', content: [{ type: 'text', text: 'Verified handoff. Next: continue.' }], api: model.api, provider: model.provider, model: model.id, stopReason: 'stop', timestamp: Date.now(), usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } }; stream.push({ type: 'done', reason: 'stop', message }); stream.end(); return stream;
    } });
    const settingsManager = SettingsManager.inMemory({ compaction: { keepRecentTokens: 256, reserveTokens: 1024 } });
    const loader = new DefaultResourceLoader({ cwd: root, agentDir: root, settingsManager, additionalExtensionPaths: [resolve(import.meta.dirname, 'index.ts')], extensionFactories: reloadInHook ? [pi => {
      pi.on('session_compact', async () => {
        if (hookEntered) return;
        hookEntered = true;
        hookReload = session.reload();
        await hookReload;
        hookCompleted = true;
      });
    }] : [], noSkills: true, noThemes: true, noPromptTemplates: true, noContextFiles: true });
    await loader.reload();
    const manager = SessionManager.create(root, join(root, 'sessions'));
    for (let i = 0; i < 12; i++) {
      manager.appendMessage({ role: 'user', content: `Synthetic goal ${i} ` + 'history '.repeat(500), timestamp: Date.now() });
      manager.appendMessage({ role: 'assistant', content: [{ type: 'text', text: 'Synthetic response.' }], api: 'cc1-fake', provider: 'cc1-fake', model: 'fake', stopReason: 'stop', timestamp: Date.now(), usage: { input: 100, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 105, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } });
    }
    ({ session } = await createAgentSession({ cwd: root, agentDir: root, modelRuntime: runtime, model: runtime.getModel('cc1-fake', 'fake'), sessionManager: manager, settingsManager, resourceLoader: loader, noTools: 'all' }));
    const errors = []; await session.bindExtensions({ onError: error => errors.push(error) }); assert.deepEqual(errors, []);
    const dirs = await readdir(runtimeDir); assert.equal(dirs.length, 1);
    const metadata = JSON.parse(await readFile(join(runtimeDir, dirs[0], 'manifest.json'), 'utf8'));
    const { socket, capabilities, handoff_directory, model_auth, ...identity } = metadata;
    assert.deepEqual(capabilities.compact, { enabled: supported, disabled_reason: supported ? null : 'unsupported_guarded_compaction', cancel_supported: supported });
    if (!supported) {
      assert.deepEqual(capabilities.handoff, { enabled: false, disabled_reason: 'unsupported_guarded_compaction', cancel_supported: false });
      // A syntactically valid protocol-2 snapshot cannot enable a missing capability.
      const expected = { managerId: randomUUID(), revision: '0', sessionId: identity.session_id, sessionFile: identity.session_file, leafId: manager.getLeafId() };
      for (const action of ['compact', 'handoff']) {
        const denied = await request(socket, { ...identity, expected, operation_id: randomUUID(), action });
        assert.equal(denied.code, 'unsupported_guarded_compaction');
        assert.equal(denied.state, 'rejected');
      }
      assert.equal(calls, 0, 'unsupported SDK never invokes any provider or legacy compact');
      assert.equal(manager.getEntries().filter(entry => entry.type === 'compaction').length, 0);
      return;
    }
    assert.equal(identity.protocol_version, 2);
    assert.deepEqual(identity.expected, manager.getGuardedCompactionState());
    if (crossLanguage) {
      // Regression: a Node-only socket fixture cannot prove Rust wire compatibility
      // or discovery/peer credentials against the real SDK extension listener.
      const before = manager.getGuardedCompactionState();
      const handoff = await rustProbe(runtimeDir, 'handoff');
      assert.equal(handoff.code, 'persisted');
      assert.deepEqual(handoff.expected_before, before);
      const artifact = await readFile(handoff.artifact_path, 'utf8');
      assert.deepEqual(JSON.parse(artifact.split('\n')[0]).expected, before);
      assert.match(artifact, /Verified handoff/);
      assert.equal(manager.getLeafId(), before.leafId);
      const compact = await rustProbe(runtimeDir, 'compact');
      assert.equal(compact.code, 'compacted');
      assert.equal(compact.compaction.capability, 'agentcenter.compactGuarded.v1');
      const persisted = (await readFile(manager.getSessionFile(), 'utf8')).trim().split('\n').map(JSON.parse);
      const entry = persisted.findLast(entry => entry.type === 'compaction');
      assert.ok(entry, 'Rust request caused native CompactionEntry persistence');
      assert.equal(entry.id, compact.compaction.entryId);
      assert.equal(entry.parentId, before.leafId);
      assert.equal(compact.compaction.previousLeafId, before.leafId);
      assert.equal(compact.expected_after.leafId, entry.id);
      assert.notEqual(compact.expected_after.revision, before.revision);
      assert.equal(manager.getLeafId(), entry.id);
      assert.equal(calls, 2, 'exactly one fake provider call per real action');
      return;
    }
    const body = { ...identity, operation_id: randomUUID(), action: 'handoff' };
    assert.equal((await request(socket, body)).state, 'accepted');
    let result;
    for (let i = 0; i < 100; i++) { result = await request(socket, { ...body, action: 'status' }); if (result.state === 'completed') break; await new Promise(resolve => setTimeout(resolve, 5)); }
    assert.equal(result.state, 'completed'); assert.equal(calls, 1);
    assert.match(await readFile(result.artifact_path, 'utf8'), /Verified handoff/);
    const artifact = await readFile(result.artifact_path, 'utf8');
    assert.deepEqual(JSON.parse(artifact.split('\n')[0]).expected, body.expected);
    assert.equal(manager.getLeafId(), body.expected.leafId);
    assert.deepEqual(result.metadata.expected, body.expected);
    const compactRequest = { ...identity, operation_id: randomUUID(), action: 'compact' };
    assert.equal((await request(socket, compactRequest)).state, 'accepted');
    if (reloadInHook) {
      for (let i = 0; i < 200 && !hookEntered; i++) await new Promise(resolve => setTimeout(resolve, 5));
      assert.equal(hookEntered, true, 'real SDK emitted persisted compact hook');
      let timer;
      try {
        await Promise.race([hookReload, new Promise((_, reject) => {
          timer = setTimeout(() => reject(new Error('reload self-waits for native compact hook')), 1000);
        })]);
      } finally { clearTimeout(timer); }
      const replacement = JSON.parse(await readFile(join(runtimeDir, (await readdir(runtimeDir))[0], 'manifest.json'), 'utf8'));
      assert.notEqual(replacement.instance_nonce, identity.instance_nonce);
      await assert.rejects(request(socket, compactRequest), /ENOENT|ECONNREFUSED/);
      const persisted = (await readFile(manager.getSessionFile(), 'utf8')).trim().split('\n').map(JSON.parse);
      assert.equal(persisted.filter(entry => entry.type === 'compaction').length, 1);
      assert.equal(calls, 2);
      return;
    }
    for (let i = 0; i < 200; i++) {
      result = await request(socket, { ...compactRequest, action: 'status' });
      if (['completed', 'failed', 'rejected', 'cancelled'].includes(result.state)) break;
      await new Promise(resolve => setTimeout(resolve, 5));
    }
    assert.equal(result.state, 'completed', JSON.stringify(result));
    assert.equal(result.code, 'compacted');
    assert.equal(result.compaction.capability, 'agentcenter.compactGuarded.v1');
    const entries = (await readFile(manager.getSessionFile(), 'utf8')).trim().split('\n').map(JSON.parse);
    const entry = entries.findLast(entry => entry.type === 'compaction');
    assert.ok(entry, 'native guarded compaction persisted in real JSONL');
    assert.equal(entry.id, result.compaction.entryId);
    assert.equal(entry.parentId, body.expected.leafId);
    assert.equal(result.compaction.previousLeafId, body.expected.leafId);
    assert.equal(result.metadata.expected.leafId, entry.id);
    assert.notEqual(result.metadata.expected.revision, body.expected.revision);
    assert.equal(manager.getLeafId(), entry.id);
    assert.equal(calls, 2);
    // Regression: session_compact fires while the operation still owns active.
    // Terminal cleanup must republish discovery, not leave it permanently busy.
    let rediscovered;
    for (let i = 0; i < 100; i++) {
      rediscovered = JSON.parse(await readFile(join(runtimeDir, (await readdir(runtimeDir))[0], 'manifest.json'), 'utf8'));
      if (rediscovered.capabilities.compact.enabled && rediscovered.capabilities.handoff.enabled) break;
      await new Promise(resolve => setTimeout(resolve, 5));
    }
    assert.equal(rediscovered.capabilities.compact.enabled, true, 'compact rediscovery leaves busy after cleanup');
    assert.equal(rediscovered.capabilities.handoff.enabled, true, 'handoff rediscovery leaves busy after cleanup');
    assert.deepEqual(rediscovered.expected, result.metadata.expected);
    const replay = await request(socket, compactRequest);
    assert.equal(replay.compaction.entryId, entry.id);
    assert.equal(calls, 2, 'replayed compact never invokes provider again');
    await session.reload();
    const replacement = JSON.parse(await readFile(join(runtimeDir, (await readdir(runtimeDir))[0], 'manifest.json'), 'utf8'));
    assert.notEqual(replacement.instance_nonce, metadata.instance_nonce);
  } finally {
    // A failed self-wait probe has already revoked its listener; do not wait again.
    if (session && (!hookEntered || hookCompleted)) await session.extensionRunner.emit({ type: 'session_shutdown', reason: 'quit' });
    session?.dispose();
    for (const [key, value] of [['XDG_RUNTIME_DIR', previous.runtime], ['XDG_STATE_HOME', previous.state]]) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
    await rm(root, { recursive: true, force: true });
  }
});
}

test('BLOCKER: public input/switch/fork/tree guards cannot prevent leaf mutation during native compact', { timeout: 5000 }, async () => {
  const root = await mkdtemp(`/run/user/${process.getuid()}/ac-cas-`);
  const key = `cc1-proof-${randomUUID()}`;
  const proof = globalThis[key] = { owned: false, inputCalls: 0 };
  let session;
  try {
    const extension = join(root, 'proof.ts');
    // Test-only opt-in extension: actual public APIs, no substituted ctx or compaction.
    await writeFile(extension, `export default function(pi) {
      const p = globalThis[${JSON.stringify(key)}];
      for (const name of ['session_before_switch','session_before_fork','session_before_tree'])
        pi.on(name, () => p.owned ? {cancel:true} : undefined);
      pi.on('input', (event) => {
        p.inputCalls++;
        if (p.owned) { p.retainedInput = event.text; return {action:'handled'}; }
      });
      pi.on('session_before_compact', (_event, ctx) => {
        if (!p.owned) return;
        p.guarded = ctx.sessionManager.getLeafId();
        if (p.guarded !== p.expected || ctx.sessionManager.getSessionId() !== p.id ||
            ctx.sessionManager.getSessionFile() !== p.file) return {cancel:true};
      });
      pi.registerCommand('native-proof', {handler: async (_args, ctx) => {
        if (!ctx.isIdle() || ctx.hasPendingMessages()) throw Error('not idle');
        p.expected = ctx.sessionManager.getLeafId(); p.id = ctx.sessionManager.getSessionId();
        p.file = ctx.sessionManager.getSessionFile(); p.owned = true;
        ctx.compact({onComplete: () => {p.completed = true; p.owned = false;},
          onError: error => {p.error = error.message; p.owned = false;}});
      }});
      pi.registerCommand('leaf-proof', {handler: async (_args, ctx) => {
        pi.appendEntry('synthetic-concurrent-extension-state', {synthetic:true});
        p.changed = ctx.sessionManager.getLeafId();
      }});
    }`);
    const runtime = await ModelRuntime.create({ credentials: AuthStorage.inMemory(), modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
    runtime.registerProvider('cc1-cas', { baseUrl: 'http://invalid.invalid', apiKey: 'synthetic', api: 'cc1-cas', models: [{ id: 'fake', name: 'fake', input: ['text'], reasoning: false, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 65536, maxTokens: 2048 }], streamSimple(model) {
      const stream = createAssistantMessageEventStream();
      proof.started = true;
      proof.release = () => {
        const message = { role: 'assistant', content: [{ type: 'text', text: 'Synthetic native compaction summary.' }], api: model.api, provider: model.provider, model: model.id, stopReason: 'stop', timestamp: Date.now(), usage: { input: 100, output: 10, cacheRead: 0, cacheWrite: 0, totalTokens: 110, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } } };
        stream.push({ type: 'done', reason: 'stop', message }); stream.end();
      };
      return stream;
    } });
    // Test-only budgets ensure prepareCompaction has a real summarized prefix.
    const settingsManager = SettingsManager.inMemory({ compaction: { keepRecentTokens: 256, reserveTokens: 1024 } });
    const loader = new DefaultResourceLoader({ cwd: root, agentDir: root, settingsManager, additionalExtensionPaths: [extension], noSkills: true, noThemes: true, noPromptTemplates: true, noContextFiles: true });
    await loader.reload();
    const manager = SessionManager.create(root, join(root, 'sessions'));
    for (let i = 0; i < 12; i++) {
      manager.appendMessage({ role: 'user', content: `Synthetic ${i} ` + 'history '.repeat(500), timestamp: Date.now() });
      manager.appendMessage({ role: 'assistant', content: [{type:'text', text:'Synthetic response.'}], api:'cc1-cas', provider:'cc1-cas', model:'fake', stopReason:'stop', timestamp:Date.now(), usage:{input:100,output:5,cacheRead:0,cacheWrite:0,totalTokens:105,cost:{input:0,output:0,cacheRead:0,cacheWrite:0,total:0}} });
    }
    ({ session } = await createAgentSession({ cwd: root, agentDir: root, modelRuntime: runtime, model: runtime.getModel('cc1-cas', 'fake'), sessionManager: manager, settingsManager, resourceLoader: loader, noTools: 'all' }));
    const errors = []; await session.bindExtensions({ onError: error => errors.push(error) });
    await session.prompt('/native-proof');
    for (let i = 0; i < 100 && !proof.started && !proof.error; i++) await new Promise(resolve => setTimeout(resolve, 5));
    assert.equal(proof.error, undefined); assert.equal(proof.started, true);
    assert.equal(proof.guarded, proof.expected);
    await session.prompt('/leaf-proof');
    assert.equal(proof.inputCalls, 0, 'extension commands bypass the public input guard');
    assert.notEqual(proof.changed, proof.expected);
    proof.release();
    for (let i = 0; i < 100 && !proof.completed && !proof.error; i++) await new Promise(resolve => setTimeout(resolve, 5));
    assert.equal(proof.error, undefined); assert.equal(proof.completed, true);
    const persisted = (await readFile(manager.getSessionFile(), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
    const entry = persisted.findLast(entry => entry.type === 'compaction');
    assert.ok(entry, 'native compaction really persisted');
    assert.equal(entry.parentId, proof.changed, 'no native revision CAS after before_compact guard');
    assert.equal(manager.getLeafId(), entry.id);
    assert.deepEqual(errors, []);
  } finally {
    proof.release?.(); session?.dispose(); delete globalThis[key];
    await rm(root, { recursive: true, force: true });
  }
});
