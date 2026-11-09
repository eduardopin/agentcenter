import { lstatSync, mkdirSync, chmodSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

// privateDirectory() rejects any group- or world-writable ancestor, so /tmp is
// out, and a socket path must stay under 100 bytes, so a deep checkout path is
// out too. Use the user runtime dir only when it is a private directory (owned
// by this user, mode 0700); containers often point XDG_RUNTIME_DIR at /tmp or a
// 0755 directory. Otherwise use a short 0700 directory in HOME.
export const privateBase = (() => {
  const runtime = process.env.XDG_RUNTIME_DIR;
  const st = runtime ? lstatSync(runtime, { throwIfNoEntry: false }) : undefined;
  if (st && st.isDirectory() && st.uid === process.getuid() && (st.mode & 0o777) === 0o700) return runtime;
  const fallback = join(homedir(), '.ac-test-rt');
  mkdirSync(fallback, { recursive: true, mode: 0o700 });
  chmodSync(fallback, 0o700);
  return fallback;
})();
