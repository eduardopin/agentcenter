import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { realpath, stat } from 'node:fs/promises';
import { isAbsolute } from 'node:path';
import { createHash } from 'node:crypto';
const execute = promisify(execFile);
// No shell, diff, file contents, configured aliases or filesystem monitor hooks.
export async function gitSnapshot(cwd) {
  try {
    if (typeof cwd !== 'string' || !isAbsolute(cwd) || cwd.includes('\0') ||
        await realpath(cwd) !== cwd || !(await stat(cwd)).isDirectory()) return { state: 'unavailable' };
    const run = async args => (await execute('git', ['--no-optional-locks', '-c', 'core.fsmonitor=false',
      '-c', 'core.untrackedCache=false', ...args], { cwd, timeout: 1500, maxBuffer: 64 * 1024,
      env: { PATH: process.env.PATH, HOME: process.env.HOME, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null', GIT_TERMINAL_PROMPT: '0' } })).stdout;
    const head = (await run(['rev-parse', '--verify', 'HEAD'])).trim();
    const branch = (await run(['rev-parse', '--abbrev-ref', 'HEAD'])).trim();
    const status = await run(['status', '--porcelain=v1', '-z', '--untracked-files=no', '--ignore-submodules=all']);
    if (!/^[0-9a-f]{40,64}$/.test(head) || !/^[^\x00-\x20\x7f]{1,256}$/.test(branch)) return { state: 'unavailable' };
    return { state: 'available', branch, head, status_hash: createHash('sha256').update(status).digest('hex'),
      scope: 'tracked_only', atomic: false };
  } catch { return { state: 'unavailable' }; }
}
