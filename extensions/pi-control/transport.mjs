import { lstat, mkdtemp, chmod, writeFile, rename, unlink, rmdir } from 'node:fs/promises';
import { createServer } from 'node:net';
import { randomBytes, timingSafeEqual } from 'node:crypto';
import { resolve, parse, join } from 'node:path';

// DAC isolates other unprivileged UIDs, not the same UID or root.
export async function privateDirectory(path) {
  const absolute = resolve(path);
  let current = parse(absolute).root;
  const uid = process.getuid();
  for (const part of absolute.slice(current.length).split('/').filter(Boolean)) {
    current = join(current, part);
    const stat = await lstat(current);
    if (!stat.isDirectory() || stat.isSymbolicLink() ||
        (stat.uid !== uid && stat.uid !== 0) || (stat.mode & 0o022)) {
      throw new Error('unsafe_directory');
    }
  }
  const stat = await lstat(absolute);
  if (stat.uid !== uid || (stat.mode & 0o777) !== 0o700) throw new Error('unsafe_directory');
  return absolute;
}

export async function listenPrivate(runtimeDirectory, identity, dispatcher) {
  const getMetadata = typeof identity === 'function' ? identity : () => identity;
  identity = getMetadata();
  const root = await privateDirectory(runtimeDirectory);
  const directory = await mkdtemp(join(root, 'ac-'));
  await chmod(directory, 0o700);
  const socket = join(directory, 's');
  const manifest = join(directory, 'manifest.json');
  const nonce = identity.instance_nonce ?? randomBytes(32).toString('hex');
  const clients = new Set();
  let closed = false;
  const metadata = { protocol_version: 2, ...identity, instance_nonce: nonce, socket,
    capabilities: identity.capabilities ?? {
      handoff: { enabled: false, disabled_reason: 'generation_not_implemented' },
      compact: { enabled: false, disabled_reason: 'unsupported_guarded_compaction' },
    } };
  function respond(client, request) {
    const current = getMetadata();
    metadata.expected = current.expected;
    metadata.capabilities = current.capabilities ?? metadata.capabilities;
    const keys = ['protocol_version', 'operation_id', 'action', 'session_id', 'session_file',
      'pid', 'process_starttime', 'instance_nonce', 'expected'];
    const reply = (state, code) => client.end(JSON.stringify({ protocol_version: 2,
      operation_id: typeof request?.operation_id === 'string' ? request.operation_id : null,
      state, code }) + '\n');
    if (!request || Array.isArray(request) || Object.keys(request).length !== keys.length ||
        keys.some(key => !Object.hasOwn(request, key)) ||
        typeof request.operation_id !== 'string' ||
        !/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(request.operation_id) ||
        !Number.isSafeInteger(request.pid) || request.pid <= 0 ||
        ['session_id', 'session_file', 'process_starttime', 'instance_nonce'].some(key => typeof request[key] !== 'string') ||
        (!request.expected || typeof request.expected !== 'object' || Array.isArray(request.expected)) ||
        !['status', 'cancel', 'handoff', 'compact'].includes(request.action)) return reply('rejected', 'invalid_request');
    if (request.protocol_version !== 2) return reply('rejected', 'incompatible');
    if (!/^[0-9a-f]{64}$/.test(request.instance_nonce) ||
        !timingSafeEqual(Buffer.from(request.instance_nonce), Buffer.from(nonce))) return reply('rejected', 'unauthorized');
    if (['session_id', 'session_file', 'pid', 'process_starttime'].some(key => request[key] !== identity[key])) return reply('rejected', 'stale_instance');
    if (!dispatcher && JSON.stringify(request.expected) !== JSON.stringify(identity.expected)) return reply('rejected', 'stale_revision');
    if (dispatcher) {
      const result = dispatcher(request);
      return client.end(JSON.stringify({ ...result, metadata: { expected: metadata.expected, capabilities: metadata.capabilities, handoff_directory: current.handoff_directory, model_auth: current.model_auth } }) + '\n');
    }
    // No effects exist in this foundation slice: unknown is never fabricated completion.
    if (request.action === 'status' || request.action === 'cancel') return reply('unknown', 'unknown_operation');
    return reply('rejected', 'disabled');
  }
  const server = createServer(client => {
    if (closed || clients.size >= 4) return client.destroy();
    clients.add(client);
    let buffer = Buffer.alloc(0);
    client.setTimeout(2000, () => client.destroy());
    client.on('error', () => {});
    client.on('close', () => clients.delete(client));
    client.on('data', chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      if (buffer.length > 16 * 1024) return client.destroy();
      const newline = buffer.indexOf(10);
      if (newline < 0) return;
      if (newline !== buffer.length - 1) return client.destroy();
      client.removeAllListeners('data');
      try { respond(client, JSON.parse(buffer.subarray(0, newline).toString('utf8'))); }
      catch { client.destroy(); }
    });
  });
  async function close() {
    if (closed) return;
    closed = true;
    for (const client of clients) client.destroy();
    if (server.listening) await new Promise(resolveClose => server.close(resolveClose));
    await unlink(manifest).catch(error => { if (error.code !== 'ENOENT') throw error; });
    await unlink(socket).catch(error => { if (error.code !== 'ENOENT') throw error; });
    await rmdir(directory);
  }
  try {
    if (Buffer.byteLength(socket) > 100) throw new Error('socket_path_too_long');
    await new Promise((ready, reject) => {
      server.once('error', reject);
      server.listen(socket, ready);
    });
    await chmod(socket, 0o600);
    await writeFile(manifest, JSON.stringify(metadata), { mode: 0o600, flag: 'wx' });
    return { manifest, socket, metadata, close, async refresh(current) {
      if (closed) return;
      if (['session_id', 'session_file', 'pid', 'process_starttime', 'instance_nonce'].some(key => current[key] !== metadata[key])) throw new Error('stale_instance');
      metadata.expected = current.expected;
      metadata.capabilities = current.capabilities;
      metadata.handoff_directory = current.handoff_directory;
      metadata.model_auth = current.model_auth;
      const next = join(directory, randomBytes(24).toString('hex'));
      try {
        await writeFile(next, JSON.stringify(metadata), { mode: 0o600, flag: 'wx' });
        await rename(next, manifest);
      } finally { await unlink(next).catch(() => {}); }
    } };
  } catch (error) {
    await close();
    throw error;
  }
}
