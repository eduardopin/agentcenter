import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { gitSnapshot } from './git-snapshot.mjs';
import { modelAuth } from './operations.mjs';

test('public auth status fails closed without resolving or returning credentials', () => {
  assert.equal(modelAuth({}), 'no_model');
  assert.equal(modelAuth({ model: { provider: 'test' }, modelRegistry: {} }), 'unavailable');
  for (const configured of [true, false]) {
    const ctx = { model: { provider: 'test' }, modelRegistry: {
      getProviderAuthStatus: provider => { assert.equal(provider, 'test'); return { configured, label: 'must not leak' }; },
      getApiKeyAndHeaders: () => assert.fail('credentials forbidden'),
    } };
    assert.equal(modelAuth(ctx), configured ? 'configured' : 'missing');
  }
});

test('git snapshot hashes tracked status only and never includes diff or file contents', async () => {
  const root = await mkdtemp('/tmp/ac-git-');
  const git = args => execFileSync('git', args, { cwd: root, stdio: 'pipe' });
  try {
    git(['init', '-q']);
    await writeFile(`${root}/tracked`, 'initial');
    git(['add', 'tracked']);
    git(['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '-qm', 'initial']);
    const clean = await gitSnapshot(root);
    assert.equal(clean.state, 'available');
    await writeFile(`${root}/tracked`, 'SECRET CONTENT NEVER EXPOSED');
    const dirty = await gitSnapshot(root);
    assert.notEqual(dirty.status_hash, clean.status_hash);
    assert.match(dirty.head, /^[0-9a-f]{40,64}$/);
    assert.equal(JSON.stringify(dirty).includes('SECRET'), false);
    assert.deepEqual(await gitSnapshot('relative'), { state: 'unavailable' });
    assert.deepEqual(await gitSnapshot(undefined), { state: 'unavailable' });
  } finally { await rm(root, { recursive: true, force: true }); }
});
