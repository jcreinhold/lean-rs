# lean-rs-writer

A standalone executable for the shared writer and reader protocols used by
`lean-toolchain` on macOS and Linux. Set `LEAN_RS_WRITER_EXECUTABLE` to
the canonical absolute path of this binary, or pass it to the SDK's
`writer_executable` builder. The SDK selects the Lake toolchain and derives
the canonical lock domains. The writer owns its child process group and
re-executes itself for the blocked launch handshake.

The same executable can serve as the worker's `native_guardian` helper.
Callers that select a private owner directory pass it to cooperating
workers and writers through the SDK's native ownership configuration.
