import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir, mkdtemp, rm, cp, symlink, unlink, lstat } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { inventory, dependencyInventory, preparePrivateRuntime, verifyPrivateRuntime } from '../prepare.mjs';
import { sourceHashes, transformSources } from '../guarded-source.mjs';

const root = resolve(import.meta.dirname, '../../..');
const prepared = join(root, 'target/pi-guarded-runtime');
const receipt = JSON.parse(await readFile(join(prepared, 'agentcenter-ready.json'), 'utf8'));
const source = receipt.source;
async function temporary(t) {
  const path = await mkdtemp(join(root, 'target/cc0-package-test-'));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}
const missing = async path => { await assert.rejects(lstat(path), { code: 'ENOENT' }); };

test('source transformer is deterministic and refuses every mismatched input and reapplication', async () => {
  const sources = {};
  for (const name of Object.keys(sourceHashes)) {
    const map = JSON.parse(await readFile(join(source, 'dist', name + '.js.map'), 'utf8'));
    sources[name] = map.sourcesContent[0];
  }
  const patched = transformSources('0.87.0', sources);
  assert.deepEqual(patched, transformSources('0.87.0', sources));
  assert.throws(() => transformSources('0.87.1', sources), /Unsupported/);
  for (const name of Object.keys(sources)) assert.throws(() => transformSources('0.87.0', { ...sources, [name]: sources[name] + '\n' }), /Pinned source mismatch/);
  assert.throws(() => transformSources('0.87.0', patched), /Pinned source mismatch/);
});

test('prepared receipt and all 1100 upstream SDK files remain verified', async () => {
  assert.equal((await verifyPrivateRuntime(prepared)).capability, 'agentcenter.compactGuarded.v1');
  assert.equal((await inventory(source)).digest, receipt.sourceDigest);
  assert.equal((await inventory(source)).count, 1100);
  assert.equal((await lstat(join(prepared, 'node_modules'))).isSymbolicLink(), false);
  assert.equal((await dependencyInventory(join(prepared, 'node_modules'))).digest,
    (await dependencyInventory(join(source, 'node_modules'))).digest);
});

test('existing output and paths outside checkout target never get overwritten', async () => {
  await assert.rejects(preparePrivateRuntime(source, prepared), /already exists/);
  await assert.rejects(preparePrivateRuntime(source, source), /inside this checkout target/);
  await assert.rejects(preparePrivateRuntime(source, join(root, 'integrations/unsafe-runtime')), /inside this checkout target/);
  assert.equal((await verifyPrivateRuntime(prepared)).outputDigest, receipt.outputDigest);
});

test('version or inventory mismatch fails before a partial output is created', async t => {
  const dir = await temporary(t), fake = join(dir, 'fake'); await mkdir(fake);
  const out = join(dir, 'output');
  await writeFile(join(fake, 'package.json'), JSON.stringify({ name: '@earendil-works/pi-coding-agent', version: '0.0.0' }));
  await assert.rejects(preparePrivateRuntime(fake, out), /Unsupported SDK/); await missing(out);
  await writeFile(join(fake, 'package.json'), JSON.stringify({ name: '@earendil-works/pi-coding-agent', version: '0.87.0' }));
  await assert.rejects(preparePrivateRuntime(fake, out), /inventory mismatch/); await missing(out);
  await symlink(join(source, 'README.md'), join(fake, 'redirected'));
  await assert.rejects(preparePrivateRuntime(fake, out), /Symlink in SDK inventory/); await missing(out);
});

test('symlinked output parent cannot redirect writes into the global SDK', async t => {
  const dir = await temporary(t), link = join(dir, 'redirect'); await symlink(source, link, 'dir');
  await assert.rejects(preparePrivateRuntime(source, join(link, 'never-create-cc0')), /Redirected output parent/);
  await missing(join(source, 'never-create-cc0'));
});

test('tampered private code fails verification and launcher refuses before CLI startup', async t => {
  const dir = await temporary(t), copy = join(dir, 'copy');
  await cp(prepared, copy, { recursive: true, dereference: false, verbatimSymlinks: true });
  await writeFile(join(copy, 'dist/core/agent-session.js'), '// tampered synthetic fixture\n');
  await assert.rejects(verifyPrivateRuntime(copy), /modified or is incomplete/);
  const result = spawnSync(process.execPath, [join(copy, 'run.mjs'), '--version'], { encoding: 'utf8', timeout: 10000,
    env: { ...process.env, PI_OFFLINE: '1', PI_TELEMETRY: '0', PI_CODING_AGENT_DIR: dir } });
  assert.equal(result.status, 1); assert.match(result.stderr, /modified or is incomplete/);
  assert.equal((await verifyPrivateRuntime(prepared)).outputDigest, receipt.outputDigest);
});

test('missing receipt, tampered dependencies and swapped dependency symlink are not usable private runtimes', async t => {
  const dir = await temporary(t), copy = join(dir, 'copy');
  await cp(prepared, copy, { recursive: true, dereference: false, verbatimSymlinks: true });
  await unlink(join(copy, 'agentcenter-ready.json'));
  await assert.rejects(verifyPrivateRuntime(copy), { code: 'ENOENT' });
  await writeFile(join(copy, 'agentcenter-ready.json'), JSON.stringify(receipt), { mode: 0o600 });
  const dependencyFile = join(copy, 'node_modules/@earendil-works/pi-ai/package.json');
  const original = await readFile(dependencyFile);
  await writeFile(dependencyFile, '// synthetic tampering\n');
  await assert.rejects(verifyPrivateRuntime(copy), /Private dependency inventory mismatch/);
  await writeFile(dependencyFile, original);
  await verifyPrivateRuntime(copy);
  await rm(join(copy, 'node_modules'), { recursive: true });
  const other = join(dir, 'other-modules'); await mkdir(other);
  await symlink(other, join(copy, 'node_modules'), 'dir');
  await assert.rejects(verifyPrivateRuntime(copy), /Dependency root must be a real directory/);
});
