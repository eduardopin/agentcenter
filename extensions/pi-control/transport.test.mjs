import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, chmod, lstat, readFile, symlink, rm } from 'node:fs/promises';
import { connect } from 'node:net';
import { randomUUID } from 'node:crypto';
import { listenPrivate, privateDirectory } from './transport.mjs';
import { privateBase } from './test-support.mjs';

const runtime = privateBase;
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

const frame = (endpoint, overrides = {}) => JSON.stringify({ protocol_version: 2,
  operation_id: randomUUID(), action: 'handoff', ...identity,
  instance_nonce: endpoint.metadata.instance_nonce, ...overrides });
// Resolves with what the server sent and whether it closed the socket, within `ms`.
function raw(endpoint, bytes, ms = 500) {
  return new Promise(resolve => {
    const socket = connect(endpoint.socket);
    let result = '', closed = false;
    socket.on('error', () => {}); // a destroyed peer may reset the write
    socket.on('data', data => { result += data; });
    socket.on('close', () => { closed = true; });
    socket.on('connect', () => { if (bytes) socket.write(bytes); });
    setTimeout(() => { const state = { result, closed }; socket.destroy(); resolve(state); }, ms);
  });
}
const open = endpoint => new Promise((resolve, reject) => {
  const socket = connect(endpoint.socket);
  socket.on('error', () => {});
  socket.once('connect', () => resolve(socket));
  socket.once('error', reject);
});
const closes = socket => new Promise(resolve => socket.destroyed ? resolve() : socket.once('close', resolve));

test('a process_starttime or session identity that differs is stale_instance', { timeout: 10_000 }, async () => {
  const endpoint = await listenPrivate(runtime, identity);
  try {
    for (const key of ['process_starttime', 'session_id', 'session_file']) {
      assert.equal((await query(endpoint, { [key]: 'other' })).code, 'stale_instance', key);
    }
  } finally { await endpoint.close(); }
});

test('frame limit is 16 KiB including the newline: exactly that is answered, one more byte is dropped', { timeout: 10_000 }, async () => {
  const endpoint = await listenPrivate(runtime, identity);
  try {
    const body = frame(endpoint);
    const exact = body + ' '.repeat(16 * 1024 - 1 - body.length) + '\n';
    assert.equal(Buffer.byteLength(exact), 16 * 1024);
    const accepted = await raw(endpoint, exact);
    assert.equal(JSON.parse(accepted.result).code, 'disabled');
    const over = await raw(endpoint, body + ' '.repeat(16 * 1024 - body.length) + '\n');
    assert.equal(over.result, '');
    assert.equal(over.closed, true);
    // The limit also holds without a newline, so a slow writer cannot grow the buffer.
    const noNewline = await raw(endpoint, ' '.repeat(16 * 1024 + 1));
    assert.equal(noNewline.closed, true);
  } finally { await endpoint.close(); }
});

test('bytes after the newline drop the connection without a reply', { timeout: 10_000 }, async () => {
  const endpoint = await listenPrivate(runtime, identity);
  try {
    const result = await raw(endpoint, frame(endpoint) + '\n{}');
    assert.deepEqual(result, { result: '', closed: true });
  } finally { await endpoint.close(); }
});

test('at most 4 connections are held; a 5th is destroyed and a freed slot is reusable', { timeout: 10_000 }, async () => {
  const endpoint = await listenPrivate(runtime, identity);
  const held = [];
  try {
    for (let i = 0; i < 4; i++) held.push(await open(endpoint));
    await new Promise(resolve => setTimeout(resolve, 50));
    const fifth = await raw(endpoint, frame(endpoint));
    assert.deepEqual(fifth, { result: '', closed: true });
    const gone = closes(held[0]);
    held[0].destroy(); await gone;
    await new Promise(resolve => setTimeout(resolve, 50));
    assert.equal((await query(endpoint)).code, 'disabled');
  } finally { for (const socket of held) socket.destroy(); await endpoint.close(); }
});

test('an idle connection is closed after 2 seconds, not before', { timeout: 10_000 }, async () => {
  const endpoint = await listenPrivate(runtime, identity);
  try {
    const socket = await open(endpoint);
    const started = Date.now();
    let early = false;
    socket.once('close', () => { early = Date.now() - started < 1500; });
    await new Promise(resolve => setTimeout(resolve, 1000));
    assert.equal(socket.destroyed, false);
    await closes(socket);
    const elapsed = Date.now() - started;
    assert.equal(early, false);
    assert.ok(elapsed < 4000, `closed after ${elapsed} ms`);
  } finally { await endpoint.close(); }
});
