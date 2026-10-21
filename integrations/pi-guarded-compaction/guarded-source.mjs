// Source-only transformations for MIT-licensed Pi 0.87.0. No filesystem writes.
import { createHash } from 'node:crypto';
export const version = '0.87.0';
export const sourceHashes = Object.freeze({
  'core/agent-session': 'e5c020bced4ada5c5e116cbd160f66e111016fc717ae30d79994a45527e7f3d7',
  'core/extensions/types': '601261b3deee1d61f85c930efed2767d66fe6c8d9637e19f4f53135b5fecf4d1',
  'core/extensions/runner': 'bf68eaf27a3e54f1cfc0982554e0598c050fd9a06c8e8b7d6cd2d8e3d8d5990c',
  'core/session-manager': 'ded5d65112f8666f0e3ae71dab39a302607c29a000745a3d06e00c2103c8f257',
  'core/extensions/index': '73f8b18ea7e72836dc90198bb74c29045f1d14571925fb706184c5eba6c7e666',
  'index': '60472823b30980d1c482f6aed19e4cab09cfeddffaa3be570d67a6cc1d7af2a0',
});
function once(text, anchor, replacement) {
  if (text.split(anchor).length !== 2) throw new Error('Non-unique/missing pinned source anchor');
  return text.replace(anchor, replacement);
}
export function transformSources(packageVersion, sources) {
  if (packageVersion !== version) throw new Error('Unsupported Pi version');
  // Validate every input before producing any transformed output.
  for (const [name, hash] of Object.entries(sourceHashes)) {
    if (typeof sources[name] !== 'string' || createHash('sha256').update(sources[name]).digest('hex') !== hash)
      throw new Error(`Pinned source mismatch: ${name}`);
  }
  const output = { ...sources };
  let manager = sources['core/session-manager'];
  manager = once(manager, 'export type ReadonlySessionManager = Pick<', `export interface GuardedCompactionState {
  managerId: string;
  revision: string;
  sessionId: string;
  sessionFile: string | undefined;
  leafId: string | null;
}

export type ReadonlySessionManager = Pick<`);
  manager = once(manager, '\t| "getLeafId"', '\t| "getLeafId"\n\t| "getGuardedCompactionState"');
  // Accessors cover every identity/leaf assignment, including failed loads and ABA.
  // Instance fields are never reset by newSession(), _loadEntries() or _buildIndex().
  manager = once(manager, '\tprivate sessionId: string = "";\n\tprivate sessionFile: string | undefined;', `  private readonly guardedManagerId = randomUUID();
  private guardedRevision = 0n;
  private _guardedSessionId = "";
  private get sessionId(): string { return this._guardedSessionId; }
  private set sessionId(value: string) { this.guardedRevision++; this._guardedSessionId = value; }
  private _guardedSessionFile: string | undefined;
  private get sessionFile(): string | undefined { return this._guardedSessionFile; }
  private set sessionFile(value: string | undefined) { this.guardedRevision++; this._guardedSessionFile = value; }
  getGuardedCompactionState(): GuardedCompactionState {
    return { managerId: this.guardedManagerId, revision: this.guardedRevision.toString(),
      sessionId: this.sessionId, sessionFile: this.sessionFile, leafId: this.leafId };
  }`);
  manager = once(manager, '\tprivate leafId: string | null = null;', `  private _guardedLeafId: string | null = null;
  private get leafId(): string | null { return this._guardedLeafId; }
  private set leafId(value: string | null) { this.guardedRevision++; this._guardedLeafId = value; }`);
  manager = once(manager, '\tappendFileSync,', '\tfsyncSync,\n\trenameSync,\n\tunlinkSync,\n\tappendFileSync,');
  const appendStart = manager.indexOf('\tappendCompaction<T = unknown>(');
  const appendEnd = manager.indexOf('\n\t/** Append a custom entry', appendStart);
  if (appendStart < 0 || appendEnd < 0) throw new Error('Missing compaction append');
  let atomic = manager.slice(appendStart, appendEnd);
  atomic = once(atomic, 'appendCompaction<T = unknown>(', 'appendGuardedCompaction<T = unknown>(\n    validate: () => void,');
  atomic = once(atomic, '\t\tthis._appendEntry(entry);', `    if (!this.persist || !this.sessionFile) throw Object.assign(new Error("non_persistent"), { code: "non_persistent" });
    const file = this.sessionFile;
    // Serialize before the final guard: extension details may invoke toJSON.
    const bytes = [...this.fileEntries, entry].map(e => JSON.stringify(e)).join("\\n") + "\\n";
    validate();
    const temporary = file + ".guarded-" + randomUUID();
    let fd: number | undefined;
    try {
      fd = openSync(temporary, "wx", 0o600);
      writeFileSync(fd, bytes);
      fsyncSync(fd);
      closeSync(fd); fd = undefined;
      validate();
      // Linearization point: atomic same-directory replacement, never partial append.
      renameSync(temporary, file);
    } finally {
      if (fd !== undefined) { try { closeSync(fd); } catch {} }
      try { unlinkSync(temporary); } catch {}
    }
    this.fileEntries.push(entry);
    this.byId.set(entry.id, entry);
    this.leafId = entry.id;
    this.flushed = true;`);
  manager = once(manager, '\tappendCompaction<T = unknown>(', atomic + '\n\tappendCompaction<T = unknown>(');
  output['core/session-manager'] = manager;
  for (const name of ['core/extensions/index', 'index']) {
    output[name] = once(sources[name], '\tCompactOptions,', '\tCompactOptions,\n\tGuardedCompactOptions,\n\tGuardedCompactMetadata,');
    output[name] += name === 'index'
      ? '\nexport type { GuardedCompactionState } from "./core/session-manager.js";\n'
      : '\nexport type { GuardedCompactionState } from "../session-manager.js";\n';
  }
  let types = sources['core/extensions/types'];
  types = once(types, '\tReadonlySessionManager,', '\tGuardedCompactionState,\n\tReadonlySessionManager,');
  types = once(types, 'export interface CompactOptions {', `export interface GuardedCompactOptions {
  expected: GuardedCompactionState;
  signal?: AbortSignal;
  customInstructions?: string;
}
export interface GuardedCompactMetadata {
  capability: "agentcenter.compactGuarded.v1";
  sessionId: string;
  sessionFile: string | undefined;
  previousLeafId: string | null;
  entryId: string;
}
export interface CompactOptions {`);
  types = once(types, '\tcompact(options?: CompactOptions): void;', '\tcompact(options?: CompactOptions): void;\n\treadonly compactGuardedCapability?: "agentcenter.compactGuarded.v1";\n\tcompactGuarded?(options: GuardedCompactOptions): Promise<GuardedCompactMetadata>;');
  types = once(types, '\tcompact: (options?: CompactOptions) => void;', '\tcompact: (options?: CompactOptions) => void;\n\tcompactGuarded?: (options: GuardedCompactOptions) => Promise<GuardedCompactMetadata>;');
  output['core/extensions/types'] = types;
  let runner = sources['core/extensions/runner'];
  runner = once(runner, '\tprivate compactFn:', '\tprivate compactGuardedFn: ExtensionContextActions["compactGuarded"];\n\tprivate compactFn:');
  runner = once(runner, '\t\tthis.compactFn = contextActions.compact;', '\t\tthis.compactFn = contextActions.compact;\n\t\tthis.compactGuardedFn = contextActions.compactGuarded;');
  runner = once(runner, '\t\t\tcompact: (options) => {', `\t\t\tget compactGuardedCapability() {
        runner.assertActive();
        return runner.compactGuardedFn ? "agentcenter.compactGuarded.v1" as const : undefined;
      },
      get compactGuarded() {
        runner.assertActive();
        const action = runner.compactGuardedFn;
        return action ? (options: Parameters<NonNullable<ExtensionContextActions["compactGuarded"]>>[0]) => {
          runner.assertActive();
          return action(options);
        } : undefined;
      },
\t\t\tcompact: (options) => {`);
  output['core/extensions/runner'] = runner;
  let session = sources['core/agent-session'];
  session = once(session, '\ttype ContextUsage,', '\ttype ContextUsage,\n\ttype GuardedCompactOptions,\n\ttype GuardedCompactMetadata,');
  const methodStart = '\tasync compact(customInstructions?: string): Promise<CompactionResult> {';
  const start = session.indexOf(methodStart);
  const end = session.indexOf('\n\t/**\n\t * Cancel in-progress compaction', start);
  if (start < 0 || end < 0) throw new Error('Missing manual compaction method');
  let method = session.slice(start, end);
  method = once(method, methodStart + '\n\t\tawait this.abort();\n\t\tthis._compactionAbortController = new AbortController();', `\tasync compactGuarded(options: GuardedCompactOptions): Promise<GuardedCompactMetadata> {
    const reject = (code: string): never => { throw Object.assign(new Error(code), { code }); };
    if (!options || typeof options !== "object" || !options.expected || typeof options.expected !== "object") reject("invalid");
    const expected = { ...options.expected };
    const signal = options.signal;
    const customInstructions = options.customInstructions;
    if (typeof expected.managerId !== "string" || !expected.managerId ||
        typeof expected.revision !== "string" || !/^(0|[1-9][0-9]*)$/.test(expected.revision) ||
        typeof expected.sessionId !== "string" || !expected.sessionId ||
        !(expected.sessionFile === undefined || typeof expected.sessionFile === "string") ||
        !(expected.leafId === null || typeof expected.leafId === "string") ||
        !(signal === undefined || signal instanceof AbortSignal) ||
        !(customInstructions === undefined || typeof customInstructions === "string")) reject("invalid");
    const manager = this.sessionManager;
    const runner = this._extensionRunner;
    const controller = new AbortController();
    const validate = (reserved = false) => {
      if (signal?.aborted || controller.signal.aborted) reject("cancelled");
      const current = manager.getGuardedCompactionState();
      if (this.sessionManager !== manager || this._extensionRunner !== runner ||
          current.managerId !== expected.managerId || current.revision !== expected.revision ||
          current.sessionId !== expected.sessionId || current.sessionFile !== expected.sessionFile ||
          current.leafId !== expected.leafId) reject("stale");
      // The context getter also checks the reload-before-rebind stale-runner window.
      try { if (runner) void runner.createContext().compactGuarded; } catch { reject("stale"); }
      if (!manager.isPersisted() || !current.sessionFile) reject("non_persistent");
      if (this.pendingMessageCount > 0 || this._pendingNextTurnMessages.length > 0 ||
          this._pendingCustomMessages.length > 0 || this._pendingBashMessages.length > 0) reject("pending");
      if (this._isAgentRunActive || this._autoCompactionAbortController || this._branchSummaryAbortController ||
          (reserved ? this._compactionAbortController !== controller : this._compactionAbortController !== undefined)) reject("busy");
    };
    validate();
    this._compactionAbortController = controller;
    const cancel = () => controller.abort();
    signal?.addEventListener("abort", cancel, { once: true });
    const sameSession = () => this.sessionManager === manager && this._extensionRunner === runner &&
      manager.getSessionId() === expected.sessionId && manager.getSessionFile() === expected.sessionFile;
    const release = () => {
      // Runner reload does not transfer ownership of this controller. Only
      // pointer identity decides whether this operation may clear its slot.
      if (this._compactionAbortController === controller) this._clearManualCompactionState();
    };
    const listeners = [...this._eventListeners];
    let terminal = false;
    const end = (event: AgentSessionEvent) => {
      if (terminal) return;
      terminal = true;
      for (const listener of listeners) { try { listener(event); } catch {} }
    };
    let committed: GuardedCompactMetadata | undefined;`);
  method = method.replaceAll('this._compactionAbortController.signal', 'controller.signal');
  method = method.replaceAll('this._extensionRunner', 'runner');
  // Restore capture/identity expressions changed by the mechanical local-reference substitution.
  method = method.replace('const runner = runner;', 'const runner = this._extensionRunner;')
    .replace('runner !== runner', 'this._extensionRunner !== runner');
  method = method.replaceAll('this.sessionManager', 'manager')
    .replace('const manager = manager;', 'const manager = this.sessionManager;')
    .replace('manager !== manager', 'this.sessionManager !== manager');
  method = once(method, 'const sameSession = () => manager === manager && runner === runner &&',
    'const sameSession = () => this.sessionManager === manager && this._extensionRunner === runner &&');
  method = once(method, '\n\t\t\tconst pathEntries', '\n      validate(true);\n\t\t\tconst pathEntries');
  method = once(method, '\n\t\t\t\tif (result?.cancel)', '\n        validate(true);\n\t\t\t\tif (result?.cancel)');
  method = once(method, '\n\t\t\t\tsummary = result.summary;', '\n        validate(true);\n\t\t\t\tsummary = result.summary;');
  method = once(method, '\t\t\tmanager.appendCompaction(summary, firstKeptEntryId, tokensBefore, details, fromExtension, usage);', `      validate(true);
      const entryId = manager.appendGuardedCompaction(() => validate(true), summary, firstKeptEntryId, tokensBefore, details, fromExtension, usage);
      committed = { capability: "agentcenter.compactGuarded.v1", sessionId: manager.getSessionId(),
        sessionFile: manager.getSessionFile(), previousLeafId: expected.leafId, entryId };`);
  method = once(method, '\t\t\tconst newEntries = manager.getEntries();', '');
  const lookup = method.indexOf('\t\t\tconst savedCompactionEntry =');
  const lookupEnd = method.indexOf('\n\n', lookup);
  method = method.slice(0, lookup) + '\t\t\tconst savedCompactionEntry = manager.getEntry(entryId) as CompactionEntry;' + method.slice(lookupEnd);
  method = method.replaceAll('this._clearManualCompactionState();', 'release();');
  method = method.replace('=== controller) release();', '=== controller) this._clearManualCompactionState();');
  // Finish local state/events before the post-commit hook can switch sessions.
  const hookStart = method.indexOf('\t\t\tif (runner && savedCompactionEntry) {');
  const hookEnd = method.indexOf('\n\n\t\t\tconst compactionResult', hookStart);
  if (hookStart < 0 || hookEnd < 0) throw new Error('Missing guarded post-commit hook');
  const hook = method.slice(hookStart, hookEnd).replace('if (runner && savedCompactionEntry)', 'if (sameSession() && runner && savedCompactionEntry)');
  method = method.slice(0, hookStart) + method.slice(hookEnd);
  method = once(method, '\t\t\treturn compactionResult;', hook + '\n\t\t\treturn compactionResult;');
  method = once(method, '\t\t\tthis._emit({\n\t\t\t\ttype: "compaction_end",\n\t\t\t\treason: "manual",\n\t\t\t\tresult: compactionResult,', '\t\t\tend({\n\t\t\t\ttype: "compaction_end",\n\t\t\t\treason: "manual",\n\t\t\t\tresult: compactionResult,');
  method = once(method, '\t\t\treturn compactionResult;', '\t\t\treturn committed;');
  method = once(method, '\t\tthis._emit({ type: "compaction_start", reason: "manual" });', '');
  method = once(method, '\t\ttry {\n\t\t\tconst model', '\t\ttry {\n      this._emit({ type: "compaction_start", reason: "manual" });\n      validate(true);\n\t\t\tconst model');
  const catchStart = method.indexOf('\t\t} catch (error) {');
  const finallyStart = method.indexOf('\t\t} finally {', catchStart);
  if (catchStart < 0 || finallyStart < 0) throw new Error('Missing guarded error boundary');
  method = method.slice(0, catchStart) + `\t\t} catch (error) {
      if (committed) return committed;
      let code = "failed";
      if (signal?.aborted || controller.signal.aborted || cancelledByExtension) code = "cancelled";
      else { try { validate(true); } catch (guard) { code = (guard as Error & { code: string }).code; } }
      const aborted = code === "cancelled";
      const errorMessage = aborted ? undefined : code;
      release();
      end({ type: "compaction_end", reason: "manual", result: undefined, aborted, willRetry: false, errorMessage });
      // Never dispatch through a replacement runner or into another session.
      if (sameSession() && runner) {
        try {
          void runner.createContext().compactGuarded;
          if (runner.hasHandlers("session_compact_failed")) await runner.emit({
            type: "session_compact_failed", reason: "manual", errorMessage, aborted, willRetry: false, fromExtension,
          });
        } catch {}
      }
      reject(code);
` + method.slice(finallyStart);
  session = once(session, methodStart, '\tget compactGuardedCapability(): "agentcenter.compactGuarded.v1" { return "agentcenter.compactGuarded.v1"; }\n\n' + methodStart);
  method = once(method, '\t\t} finally {\n\t\t\trelease();', '\t\t} finally {\n      signal?.removeEventListener("abort", cancel);\n\t\t\trelease();');
  session = once(session, methodStart, method + '\n' + methodStart);
  session = once(session, '\t\t\t\tgetSystemPrompt: () => this.systemPrompt,', '\t\t\t\tcompactGuarded: (options) => this.compactGuarded(options),\n\t\t\t\tgetSystemPrompt: () => this.systemPrompt,');
  output['core/agent-session'] = session;
  return output;
}
