import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, rm, readdir, lstat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';
import { inventory, verifyPrivateRuntime } from '../prepare.mjs';

const root = resolve(import.meta.dirname, '../../..');
const runtime = join(root, 'target/pi-guarded-runtime');

// Inspect only a synthetic unrelated profile, never live harness storage.
// A live profile changes concurrently and cannot prove attribution of writes.
async function storageSnapshot(path) {
  try {
    const stat = await lstat(path);
    const result = [[path, stat.mode, stat.size, stat.mtimeMs]];
    if (stat.isDirectory()) {
      for (const name of (await readdir(path)).sort()) result.push(...await storageSnapshot(join(path, name)));
    }
    return result;
  } catch (error) {
    if (error.code === 'ENOENT') return [];
    throw error;
  }
}

test('opt-in unbundled CLI loads real guarded ctx capability and session identity offline', { timeout: 15000 }, async () => {
  // Regression: patching SDK core alone must not count as proof of the actual CLI entrypoint.
  const receipt = await verifyPrivateRuntime(runtime);
  const upstream = JSON.parse(await readFile(join(runtime, 'agentcenter-ready.json'), 'utf8')).source;
  const globalBefore = await inventory(upstream);
  const dir = await mkdtemp(join(root, 'target/cc0-cli-smoke-'));
  const unrelatedProfile = join(dir, 'unrelated-profile');
  await mkdir(unrelatedProfile);
  await writeFile(join(unrelatedProfile, 'synthetic-marker.json'), '{"fixture":true}\n');
  const storageBefore = await storageSnapshot(unrelatedProfile);
  let child;
  let closed = false;
  let spawnError;
  let stdoutBytes = 0;
  let stderrBytes = 0;
  let overflow = false;
  try {
    const home = join(dir, 'home'), config = join(dir, 'config'), proof = join(dir, 'proof.json');
    await mkdir(home); await mkdir(config);
    const probe = join(dir, 'probe.ts');
    await writeFile(probe, `import { writeFileSync } from 'node:fs';
export default function(pi) {
  pi.on('session_start', (_event, ctx) => {
    writeFileSync(${JSON.stringify(proof)}, JSON.stringify({
      capability: ctx.compactGuardedCapability,
      hasGuarded: typeof ctx.compactGuarded === 'function',
      state: ctx.sessionManager.getGuardedCompactionState()
    }));
  });
}
`);
    child = spawn(process.execPath, [join(runtime, 'run.mjs'), '--offline', '--mode', 'rpc', '--no-session',
      '--no-extensions', '-e', probe, '--no-skills', '--no-themes', '--no-context-files', '--no-tools'], {
      cwd: dir, stdio: ['pipe', 'pipe', 'pipe'],
      env: { PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: config,
        XDG_CONFIG_HOME: config, XDG_CACHE_HOME: join(dir, 'cache'),
        PI_OFFLINE: '1', PI_TELEMETRY: '0', NO_COLOR: '1' },
    });
    child.on('error', error => { spawnError = error.code; });
    child.on('close', () => { closed = true; });
    child.stdin.on('error', () => {});
    const count = stream => chunk => {
      if (stream === 'stdout') stdoutBytes += chunk.length; else stderrBytes += chunk.length;
      if (stdoutBytes + stderrBytes > 65536) { overflow = true; child.kill('SIGTERM'); }
    };
    child.stdout.on('data', count('stdout')); child.stderr.on('data', count('stderr'));
    const deadline = Date.now() + 10000;
    let observed;
    while (Date.now() < deadline && !overflow && !spawnError) {
      try { observed = JSON.parse(await readFile(proof, 'utf8')); break; }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      if (closed) break;
      await delay(25);
    }
    const diagnostic = `CLI proof missing: exit=${child.exitCode}, signal=${child.signalCode}, spawn=${spawnError}, stdoutBytes=${stdoutBytes}, stderrBytes=${stderrBytes}, overflow=${overflow}`;
    assert.ok(observed, diagnostic);
    assert.equal(overflow, false, diagnostic);
    assert.equal(observed.hasGuarded, true);
    assert.equal(observed.capability, 'agentcenter.compactGuarded.v1');
    assert.equal(receipt.capability, 'agentcenter.compactGuarded.v1');
    assert.match(observed.state.managerId, /^[0-9a-f-]{36}$/i);
    assert.match(observed.state.sessionId, /^[0-9a-f-]{36}$/i);
    assert.match(observed.state.revision, /^\d+$/);
    // CLI initialization can append model/thinking settings before session_start.
    assert.ok(observed.state.leafId === null || /^[0-9a-f]{8}$/i.test(observed.state.leafId));
    assert.equal(observed.state.sessionFile, undefined, '--no-session must not persist a real session');
  } finally {
    if (child && !closed) {
      child.stdin.end();
      const eofDeadline = Date.now() + 300;
      while (!closed && Date.now() < eofDeadline) await delay(20);
      if (!closed) child.kill('SIGTERM'); // Launcher forwards TERM to its own CLI child.
      const termDeadline = Date.now() + 2000;
      while (!closed && Date.now() < termDeadline) await delay(20);
    }
    const storageAfter = await storageSnapshot(unrelatedProfile);
    await rm(dir, { recursive: true, force: true });
    assert.ok(!child || closed, 'own CLI wrapper must close after EOF/TERM; no background delegation');
    assert.deepEqual(await inventory(upstream), globalBefore, 'global SDK/CLI files remain intact');
    assert.deepEqual(storageAfter, storageBefore, 'unrelated synthetic profile remains intact');
  }
});
