// Consumer-only compile check; skipLibCheck does not typecheck the whole SDK.
import type { AgentSession, ExtensionContext, GuardedCompactOptions, GuardedCompactMetadata, GuardedCompactionState } from '../../../target/pi-guarded-runtime/dist/index.js';
export async function compact(ctx: ExtensionContext, session: AgentSession, manager: ExtensionContext['sessionManager']): Promise<GuardedCompactMetadata | undefined> {
  const expected: GuardedCompactionState = manager.getGuardedCompactionState();
  const options: GuardedCompactOptions = { expected, signal: new AbortController().signal, customInstructions: 'Synthetic' };
  if (ctx.compactGuardedCapability !== 'agentcenter.compactGuarded.v1' || !ctx.compactGuarded) return undefined;
  const preOperationCapability: 'agentcenter.compactGuarded.v1' = session.compactGuardedCapability;
  void preOperationCapability;
  const result: GuardedCompactMetadata = await ctx.compactGuarded(options);
  const capability: 'agentcenter.compactGuarded.v1' = result.capability;
  void capability;
  // @ts-expect-error identity must include manager/revision/session/file/leaf
  session.compactGuarded({ expected: { sessionId: 'synthetic' } });
  // @ts-expect-error cancellation requires AbortSignal, not controller
  session.compactGuarded({ expected, signal: new AbortController() });
  return session.compactGuarded(options);
}
