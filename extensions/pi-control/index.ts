import { mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { createOperations } from './operations.mjs';
import { listenPrivate, privateDirectory } from './transport.mjs';

// Explicit opt-in extension. Factory registers handlers only, never touches disk.
export default function piControl(pi) {
  let context, operations, listener, deferredRefresh;
  function clearDeferredRefresh() {
    clearTimeout(deferredRefresh);
    deferredRefresh = undefined;
  }
  let lifecycle = Promise.resolve();
  let deferredPublication = Promise.resolve();
  async function close() {
    clearDeferredRefresh();
    const previousListener = listener, previousOperations = operations;
    listener = operations = undefined;
    // Drain an already-started timer write before removing its private directory.
    await deferredPublication;
    // Revoke discovery/connections before waiting for cooperative cancellation.
    await previousListener?.close();
    await previousOperations?.close();
  }
  pi.on('session_start', (_event, ctx) => {
    lifecycle = lifecycle.catch(() => {}).then(async () => {
      await close();
      context = ctx;
      const state = process.env.XDG_STATE_HOME || join(homedir(), '.local', 'state');
      const outputRoot = join(state, 'agentcenter', 'handoffs');
      await mkdir(outputRoot, { recursive: true, mode: 0o700 });
      await privateDirectory(outputRoot);
      let ownedListener;
      operations = await createOperations({ getContext: () => context, outputRoot,
        onChange: current => ownedListener?.refresh(current) });
      try {
        listener = ownedListener = await listenPrivate(process.env.XDG_RUNTIME_DIR, operations.metadata, operations.dispatch);
      } catch (error) { await close(); throw error; }
    });
    return lifecycle;
  });
  pi.on('session_shutdown', () => {
    lifecycle = lifecycle.catch(() => {}).then(close);
    return lifecycle;
  });
  // Query responses always use live getters; events also refresh discovery on disk.
  for (const event of ['agent_start', 'agent_end', 'agent_settled', 'message_end', 'session_tree', 'session_compact', 'model_select']) {
    pi.on(event, async (_event, ctx) => {
      const ownedLifecycle = lifecycle;
      await ownedLifecycle;
      if (lifecycle !== ownedLifecycle || !listener) return;
      context = ctx;
      const ownedListener = listener, ownedOperations = operations;
      clearDeferredRefresh();
      await ownedListener.refresh(ownedOperations.metadata());
      // agent_end precedes runtime settlement. One deferred publication, never polling;
      // agent_settled publishes the final state even if another hook delays settlement.
      if (event === 'agent_end' && listener === ownedListener && lifecycle === ownedLifecycle) {
        deferredRefresh = setTimeout(() => {
          deferredRefresh = undefined;
          if (listener !== ownedListener || operations !== ownedOperations || lifecycle !== ownedLifecycle) return;
          deferredPublication = ownedListener.refresh(ownedOperations.metadata()).catch(() => {});
        }, 0);
        deferredRefresh.unref();
      }
    });
  }
}
