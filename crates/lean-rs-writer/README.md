# lean-rs-writer

The shared Lake writer and read-custody helper that `lean-toolchain` drives on macOS and Linux, as a binary and as a
library.

## The binary

Set `LEAN_RS_WRITER_EXECUTABLE` to the canonical absolute path of the `lean-rs-writer` binary, or pass it to the SDK's
`writer_executable` builder. The SDK selects the Lake toolchain and derives the canonical lock domains. The writer owns
each command's process group and re-executes itself for the blocked launch handshake. The same executable serves as the
worker's native guardian. Callers that keep a private owner directory pass it to cooperating workers and writers through
the SDK's native ownership settings.

| Subcommand | Use |
| --- | --- |
| `writer ROOT LOCK ADMISSION` | Hold the project write locks and run the commands `SharedWriter` sends on stdin. |
| `writer-lock-path ROOT` | Print the protocol, project root and lock path as JSON without taking the lock. |
| `reader`, `read-proxy` | Hold shared read custody for `SharedReader` and `SharedReadProxy`. |

## In a host binary

A host binary can serve these subcommands itself instead of shipping this one. Flatten `HelperCommand` into its own
subcommands and call `HelperCommand::run`; its documentation shows the pattern. Before dispatch, a host that keeps its
owner directory somewhere other than `LEAN_RS_WORKER_OWNERS_DIRECTORY` or the SDK default binds it with
`lean_toolchain::native_guardian::bind_registry_directory`. Launch writers with `contract()?.executable`. A process that
does not serve the subcommands, such as a test harness, names the binary that does with `use_helper_executable` first.

## Stage output

Each stage's stdout and stderr go to a 64 KiB capture that comes back in its `Output`. A stage that prints more is
stopped and reports no result. To keep a long build's output, set `log::LOG_ENV` (`LEAN_RS_WRITER_LOG`, an absolute
path) and `log::LOG_LIMIT_ENV` (`LEAN_RS_WRITER_LOG_LIMIT`, a byte count) on the command. The helper appends both
streams to that file until it holds the cap, then writes one truncation line and keeps reading, so the stage runs to the
end. The `Output` then has empty stdout and stderr, and the stage never sees either setting. `log::CappedLog` adds the
caller's own lines to the same file under the same cap. The helper refuses the stage before launch, as
`WriterDisposition::NotStarted`, when a setting is missing or malformed or the file cannot be opened as a regular file;
it never follows a symbolic link.
