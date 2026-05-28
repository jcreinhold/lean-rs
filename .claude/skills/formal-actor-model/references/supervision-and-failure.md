# Supervision And Failure

Actor supervision is not just restarting tasks. It is the public failure semantics of the system.

## Supervisor Responsibilities

- Own child lifecycle.
- Start children in dependency order.
- Observe child exits.
- Classify exits as normal, shutdown, failure, timeout, cancellation, or killed.
- Apply a restart strategy.
- Enforce restart intensity limits.
- Escalate when local recovery is not safe.
- Publish generation/freshness after restart.

## Restart Strategies

Common strategies:

- `one_for_one`: restart only the failed child.
- `one_for_all`: stop all siblings, then restart the group.
- `rest_for_one`: stop children started after the failed child, then restart that suffix.
- `temporary`: never restart this child.
- `transient`: restart only on abnormal exit.
- `permanent`: always restart.

Choose based on state dependency, not convenience. If actors share protocol state, restarting just one may violate
invariants.

## Restart Intensity

Track restarts in a time window:

```text
restart allowed iff restarts_in_window < max_restarts
```

When exceeded:

- stop the child or subtree;
- mark supervisor unhealthy;
- return structured `restart_limit_exceeded`;
- escalate to parent if one exists.

This prevents infinite crash loops that consume memory, CPU, and logs.

## Failure Taxonomy

Use structured statuses:

- `normal`: actor completed intended work.
- `stopped`: caller or supervisor requested shutdown.
- `cancelled`: runtime cancellation interrupted work.
- `timeout`: operation exceeded a budget.
- `mailbox_closed`: message could not be sent because receiver is gone.
- `mailbox_full`: bounded queue rejected or delayed work.
- `panic`: Rust panic or equivalent unexpected failure.
- `worker_exited`: child process died.
- `restart_limit_exceeded`: supervisor refused further restart.
- `unavailable`: actor identity exists but no current instance can handle work.

Avoid collapsing all failures into `io::Error` or `String`.

## Retry Semantics

Retries are safe only when the operation is idempotent or deduplicated.

For request/reply:

- assign correlation IDs;
- record whether the request was accepted;
- distinguish "not sent", "queued", "processing unknown", and "processed but reply lost" when possible;
- let business logic define idempotence.

Do not retry automatically across FFI if ownership or side effects are unclear.

## Worker Death

When a worker process dies:

- mark all in-flight calls as failed with a retryable or non-retryable status;
- restart only through the supervisor;
- bump generation/freshness;
- invalidate cached handles/snapshots as needed;
- report whether caller should retry.

Do not let callers believe a request succeeded just because the host process is still alive.

## Shutdown

A good shutdown path:

1. Stop accepting new messages.
2. Drain, reject, or drop existing messages according to policy.
3. Ask children to stop in reverse dependency order.
4. Wait within a bounded deadline.
5. Force stop if supported.
6. Report final status.

Make the drain/drop policy explicit; it affects user-visible semantics.
