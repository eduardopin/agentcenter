# Guarded compaction contract v1

CC0 supplies a private Pi 0.87.0 API for CC1. It does not supply the control
socket, Rust client or GUI buttons. Integration and activation remain separate
work. Preparation, offline checks and reversal are documented in the
[integration README](../integrations/pi-guarded-compaction/README.md).

## Detect before dispatch

Both `AgentSession` and extension `ctx` expose
`compactGuardedCapability === "agentcenter.compactGuarded.v1"`. The context
method is optional. A consumer must check the exact capability and methods
before enabling an action. A stale context can throw while being inspected;
treat that as unavailable and rediscover the current context.

```typescript
// ctx is from the private runtime, not an existing installed Pi session.
if (ctx.compactGuardedCapability !== "agentcenter.compactGuarded.v1" ||
    typeof ctx.compactGuarded !== "function" ||
    typeof ctx.sessionManager.getGuardedCompactionState !== "function") {
  throw new Error("unsupported_guarded_compaction");
}
const expected = ctx.sessionManager.getGuardedCompactionState();
// Preserve this exact snapshot across user confirmation. Do not silently refresh it.
const controller = new AbortController();
const result = await ctx.compactGuarded({ expected, signal: controller.signal });
// result contains identity and entryId, never summary or transcript content.
```

The installed SDK has no such API. Never fall back to `ctx.compact`, prompts,
keystrokes, `ctx.abort`, private methods or monkeypatches.

## Request and response

`GuardedCompactOptions` accepts `expected`, optional `signal: AbortSignal` and
optional `customInstructions: string`. `expected` is `GuardedCompactionState`:

| Field | Meaning |
| --- | --- |
| `managerId: string` | Random UUID for this manager instance |
| `revision: string` | Monotonic decimal BigInt counter, not a JSON number |
| `sessionId: string` | Session identity |
| `sessionFile: string \| undefined` | File identity; persistence is required |
| `leafId: string \| null` | Confirmed branch position |

Manager identity survives neither replacement nor process restart. Revision
increases on identity and leaf assignments, including branch-away/branch-back
and same-value assignments. Obtain it through the public getter, not by
constructing a value from a transcript. The state and option types are exported
from the SDK and extension barrels.

The resolved `GuardedCompactMetadata` has `capability`, `sessionId`,
`sessionFile`, `previousLeafId` and `entryId`. Metadata is captured at commit;
a later hook changing sessions does not rename that completed operation.

Rejections have a sanitized `Error.message` equal to `Error.code`:

| Code | Condition and caller action |
| --- | --- |
| `invalid` | Malformed options; fix the request |
| `stale` | Identity, revision, leaf or runner changed; rediscover and reconfirm |
| `pending` | Agent, custom, next-turn or bash message queue is nonempty; wait |
| `busy` | Agent run, compaction or branch summary owns the runtime; wait |
| `non_persistent` | Memory-only manager or no file; action is unsupported |
| `cancelled` | This signal/controller or a compaction hook cancelled before commit |
| `failed` | Other pre-commit failure, including provider, preparation or I/O |

If several guards fail, cancellation precedes identity, persistence, queues and
busy checks. Hook exceptions retain Pi's extension error routing; they may be
reported to the extension error callback while default summarization continues.
They are not automatically interpreted as a cancellation.

## Commit and ownership

Guards run before reservation or provider effects, after authentication,
hooks and summary generation, and before persistence. Guarded compaction never
calls `abort()` to interrupt another turn. The reservation and cancellation
controller belong to this operation; late cleanup clears only its own slot.

The guarded manager path serializes the complete JSONL history plus the new
entry to a private same-directory temporary file, flushes that file and then
revalidates. Synchronous rename is the commit point. Only after rename succeeds
does it publish the entry, leaf and context in memory. A pre-commit failure
leaves those unchanged. There is no awaited gap between final guard and rename.
Legacy `appendCompaction`, manual `compact` and automatic compaction retain their
original implementation.

Cancellation after commit cannot undo the entry and resolves as completed.
The SDK emits the guarded terminal event before awaiting post-commit extension
hooks. Failure events carry codes, not provider error content. Normal Pi
success events and extension hooks still carry compaction content locally;
consumers must not forward them to AgentCenter snapshots or logs. A slow hook
or provider can delay Promise settlement; cancellation is cooperative, not a
hard kill or a deadline guarantee. A client timeout is an unknown outcome,
not permission to retry a write blindly. CC1 owns operation IDs and reconciliation.

## Limits

This is in-process compare-and-commit, not a filesystem lock against other
processes or multiple managers independently writing the same file. Session
files require a single writer. Mutating raw entry/header references, overriding
methods, hostile extensions, same-UID attackers and root are outside the guard.
Use public append/navigation APIs; revision does not observe arbitrary object
mutation. Runtime checksums detect accidental changes under a trusted checkout;
they are not signed supply-chain attestation or an OS sandbox.

Atomic replacement rewrites the full transcript synchronously, costs O(history
bytes), changes its inode and requires temporary disk space for another copy.
It fsyncs the temporary file, not its parent directory; survival across power
loss is not promised. Crash recovery between rename and memory publication
requires reopening the file to reconcile its entry. No rejected model work is
refunded, and cancellation does not roll back independent hook side effects.

Offline tests exercise the real private SDK with synthetic credentials and a
fake provider, including the legacy no-CAS control. They do not establish live
provider behavior, full upstream source type safety or GUI performance. The CLI
smoke test proves the unbundled RPC entrypoint loads the API without a model
request, not interactive visual QA. No global installation, existing-session
restart, real session control, publication or automatic handoff/session switch
is part of CC0.
