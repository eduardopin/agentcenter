import { existsSync, mkdirSync, chmodSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

// privateDirectory() rejects any group- or world-writable ancestor, so /tmp is
// out, and a socket path must stay under 100 bytes, so a deep checkout path is
// out too. Use the user runtime dir when it exists, else a short 0700 directory
// in HOME, so a clean runner needs no /run/user/UID.
export const privateBase = (() => {
  const runtime = process.env.XDG_RUNTIME_DIR;
  if (runtime && existsSync(runtime)) return runtime;
  const fallback = join(homedir(), '.ac-test-rt');
  mkdirSync(fallback, { recursive: true, mode: 0o700 });
  chmodSync(fallback, 0o700);
  return fallback;
})();
