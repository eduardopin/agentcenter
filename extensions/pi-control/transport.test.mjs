import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, chmod, lstat, readFile, symlink, rm } from 'node:fs/promises';
import { connect } from 'node:net';
import { randomUUID } from 'node:crypto';
import { listenPrivate, privateDirectory } from './transport.mjs';

const runtime = `/run/user/${process.getuid()}`;
const identity = { session_id: 'synthetic', session_file: '/synthetic/session.jsonl',
  pid: process.pid, process_starttime: 'synthetic-start', expected: { managerId: 'manager', revision: '1', sessionId: 'synthetic', sessionFile: '/synthetic/session.jsonl', leafId: 'leaf' } };
function query(endpoint, overrides = {}) {
  return new Promise((resolve, reject) => {
    const socket = connect(endpoint.socket);
    let result = '';
    socket.on('error', reject);
    socket.on('data', data => { result += data; });
    socket.on('end', () => resolve(JSON.parse(result)));
    socket.on('connect', () => socket.write(JSON.stringify({ protocol_version: 2,
      operation_id: randomUUID(), action: 'handoff', ...identity,
      instance_nonce: endpoint.metadata.instance_nonce, ...overrides }) + '\n'));
  });
}

test('real socket is private; disabled actions never claim completion; cleanup is idempotent', async () => {
  const endpoint = await listenPrivate(runtime, identity);
  try {
    assert.equal((await lstat(endpoint.socket)).mode & 0o777, 0o600);
    assert.equal((await lstat(endpoint.manifest)).mode & 0o777, 0o600);
    const metadata = JSON.parse(await readFile(endpoint.manifest, 'utf8'));
    assert.equal(metadata.capabilities.compact.enabled, false);
    assert.equal((await query(endpoint)).code, 'disabled');
    assert.equal((await query(endpoint, { action: 'compact' })).state, 'rejected');
    assert.equal((await query(endpoint, { action: 'status' })).state, 'unknown');
    assert.equal((await query(endpoint, { action: 'cancel' })).state, 'unknown');
    assert.equal((await query(endpoint, { instance_nonce: 'x'.repeat(64) })).code, 'unauthorized');
    assert.equal((await query(endpoint, { expected: { ...identity.expected, revision: '2' } })).code, 'stale_revision');
    assert.equal((await query(endpoint, { pid: process.pid + 1 })).code, 'stale_instance');
    assert.equal((await query(endpoint, { protocol_version: 1 })).code, 'incompatible');
    assert.equal((await query(endpoint, { prompt: 'forbidden' })).code, 'invalid_request');
  } finally { await endpoint.close(); await endpoint.close(); }
  await assert.rejects(lstat(endpoint.manifest), { code: 'ENOENT' });
});

test('well-formed wrong nonce is unauthorized before the dispatcher runs', async () => {
  let calls = 0;
  const dispatcher = request => { calls++; return { protocol_version: 2, operation_id: request.operation_id, state: 'accepted', code: 'accepted' }; };
  const endpoint = await listenPrivate(runtime, identity, dispatcher);
  try {
    const real = endpoint.metadata.instance_nonce;
    const wrong = real.slice(0, -1) + (real.at(-1) === '0' ? '1' : '0');
    assert.match(wrong, /^[0-9a-f]{64}$/);
    assert.notEqual(wrong, real);
    for (const action of ['handoff', 'compact', 'status', 'cancel']) {
      const result = await query(endpoint, { action, instance_nonce: wrong });
      assert.deepEqual([result.state, result.code], ['rejected', 'unauthorized'], action);
    }
    assert.equal(calls, 0);
    // The same request with the real nonce reaches the dispatcher: the spy is live.
    assert.equal((await query(endpoint)).code, 'accepted');
    assert.equal(calls, 1);
  } finally { await endpoint.close(); }
});

test('rejects symlink and permissive runtime directory before publishing', async () => {
  const directory = await mkdtemp(`${runtime}/ac-test-`);
  try {
    await chmod(directory, 0o700);
    await symlink(directory, `${directory}/alias`);
    await assert.rejects(privateDirectory(`${directory}/alias`), /unsafe_directory/);
    await chmod(directory, 0o755);
    await assert.rejects(listenPrivate(directory, identity), /unsafe_directory/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
