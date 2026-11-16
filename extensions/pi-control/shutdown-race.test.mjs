import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readdir, writeFile, rm, lstat } from 'node:fs/promises';
import { connect } from 'node:net';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { listenPrivate } from './transport.mjs';
import { privateBase } from './test-support.mjs';

const identity = { session_id: 'synthetic', session_file: '/synthetic/session.jsonl',
  pid: process.pid, process_starttime: 'synthetic-start', expected: { managerId: 'manager', revision: '1', sessionId: 'synthetic', sessionFile: '/synthetic/session.jsonl', leafId: 'leaf' } };

// refresh() serialises the manifest synchronously after its `closed` check and
// before it queues the temp-file write, so a toJSON hook on a metadata field
// runs close() at exactly that point, with no timing involved.
test('close() that starts between refresh() closed-check and its temp write leaves no directory and does not throw', async () => {
  const runtime = await mkdtemp(join(privateBase, 'shut-race-'));
  const endpoint = await listenPrivate(runtime, identity);
  const directory = join(endpoint.manifest, '..');
  let closing;
  const handoff_directory = { toJSON() { closing = endpoint.close(); closing.catch(() => {}); return '/synthetic'; } };
  try {
    const refreshing = endpoint.refresh({ ...identity, instance_nonce: endpoint.metadata.instance_nonce, handoff_directory });
    assert.ok(closing, 'close() was injected inside refresh()');
    await refreshing.catch(() => {});
    await assert.doesNotReject(closing, 'close() must tolerate a temp file written by the racing refresh');
    await assert.rejects(lstat(directory), { code: 'ENOENT' }, 'listener directory is removed');
  } finally {
    await closing?.catch(() => {});
    await rm(runtime, { recursive: true, force: true });
  }
});

// Drives the real index.ts with a minimal fake pi/ctx. A stray file in the
// listener directory makes listener.close() throw ENOTEMPTY, the same failure
// the refresh race produces, without depending on the race.
test('session_shutdown closes operations (clears the 60 s deadline timer) even when listener close throws', async () => {
  const root = await mkdtemp(join(privateBase, 'shut-idx-'));
  const runtimeDir = join(root, 'runtime');
  await mkdir(runtimeDir, { mode: 0o700 });
  const old = { XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR, XDG_STATE_HOME: process.env.XDG_STATE_HOME };
  process.env.XDG_RUNTIME_DIR = runtimeDir;
  process.env.XDG_STATE_HOME = root;
  const realSet = globalThis.setTimeout, realClear = globalThis.clearTimeout;
  const deadlines = [];
  globalThis.setTimeout = (fn, delay, ...rest) => {
    const handle = realSet(fn, delay, ...rest);
    if (delay === 60_000) deadlines.push({ handle, cleared: false });
    return handle;
  };
  globalThis.clearTimeout = handle => {
    for (const entry of deadlines) if (entry.handle === handle) entry.cleared = true;
    return realClear(handle);
  };
  try {
    const handlers = new Map();
    const pi = { on: (event, handler) => handlers.set(event, handler), registerCommand() {} };
    const { default: piControl } = await import('./index.ts');
    piControl(pi);
    let streamStarted;
    const started = new Promise(resolve => { streamStarted = resolve; });
    const manager = { getSessionId: () => 'synthetic', getSessionFile: () => '/synthetic/session.jsonl',
      getGuardedCompactionState: () => identity.expected, getHeader: () => ({}),
      buildSessionProjection: () => ({ messages: [] }) };
    const ctx = { cwd: root, model: { provider: 'fake' }, sessionManager: manager,
      compactGuardedCapability: 'agentcenter.compactGuarded.v1', compactGuarded: async () => ({}),
      isIdle: () => true, hasPendingMessages: () => false,
      modelRegistry: { getProviderAuthStatus: () => ({ configured: true }),
        streamSimple: () => { streamStarted(); return { result: () => new Promise(() => {}) }; } } };
    await handlers.get('session_start')({}, ctx);
    const directory = join(runtimeDir, (await readdir(runtimeDir))[0]);
    const manifest = JSON.parse(await (await import('node:fs/promises')).readFile(join(directory, 'manifest.json'), 'utf8'));
    const { socket, capabilities, handoff_directory, model_auth, ...rest } = manifest;
    const reply = await new Promise((resolve, reject) => {
      const client = connect(socket);
      let body = '';
      client.on('error', reject);
      client.on('data', chunk => { body += chunk; });
      client.on('end', () => resolve(JSON.parse(body)));
      client.on('connect', () => client.end(JSON.stringify({ ...rest, operation_id: randomUUID(),
        action: 'handoff', expected: identity.expected }) + '\n'));
    });
    assert.equal(reply.state, 'accepted');
    await started;
    assert.equal(deadlines.length, 1, 'in-flight operation armed its 60 s deadline');
    await writeFile(join(directory, 'stray'), 'x');
    await handlers.get('session_shutdown')({}).catch(() => {});
    assert.equal(deadlines[0].cleared, true, 'operations.close() ran and the deadline timer was cleared');
  } finally {
    for (const entry of deadlines) realClear(entry.handle);
    globalThis.setTimeout = realSet;
    globalThis.clearTimeout = realClear;
    for (const [key, value] of Object.entries(old)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
    await rm(root, { recursive: true, force: true });
  }
});
