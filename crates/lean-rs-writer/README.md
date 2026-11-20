# lean-rs-writer

A standalone executable for the shared writer and reader protocols used by
`lean-toolchain` on macOS and Linux. Set `LEAN_HOST_MCP_WRITER_EXECUTABLE` to
the canonical absolute path of this binary, or pass it to the SDK's
`writer_executable` builder. The SDK selects the Lake toolchain and derives
the canonical lock domains. The writer owns its child process group and
re-executes itself for the blocked launch handshake.
