# cargo-mcp

An [MCP](https://modelcontextprotocol.io) server that lets an AI assistant run
`cargo check`, `cargo clippy` and `cargo test` on a local Rust project and get
structured results back (diagnostics with file/line/column and suggested fixes,
test counts, and the output of failing tests) instead of raw terminal text.

It talks MCP over stdio and is built on the official Rust SDK,
[`rmcp`](https://crates.io/crates/rmcp).

## Requirements

- A Rust toolchain with `cargo` on `PATH` (the server runs whichever `cargo`
  the `CARGO` environment variable points to, falling back to `cargo`).
- `clippy` installed for `cargo_clippy` (`rustup component add clippy`).
- Linux or macOS. On other platforms it builds, but a timeout only kills
  `cargo` itself, not the processes it spawned.

## Build and install

```sh
cargo build --release
# binary: target/release/cargo-mcp

# or install it into ~/.cargo/bin
cargo install --path .
```

## Connect it to a client

### Claude Code

```sh
claude mcp add cargo-mcp -- /absolute/path/to/cargo-mcp
# with a custom default timeout:
claude mcp add cargo-mcp -- /absolute/path/to/cargo-mcp --timeout-secs 300
```

### Clients configured with JSON (Claude Desktop, Cursor, …)

```json
{
  "mcpServers": {
    "cargo-mcp": {
      "command": "/absolute/path/to/cargo-mcp",
      "args": ["--timeout-secs", "300"]
    }
  }
}
```

## Configuration

Every cargo run has a timeout. When it expires, cargo and everything it started
(rustc, build scripts, test binaries) are killed and the tool returns an error.

| Setting | Where | Default |
|---|---|---|
| Default timeout | `--timeout-secs <N>` flag, or `CARGO_MCP_TIMEOUT_SECS` env var | 120 s |
| Per-call timeout | `timeout_secs` tool argument (clamped to 1 s – 1 h) | server default |

The flag takes precedence over the environment variable.

## Tools

All three tools take the same arguments:

| Argument | Type | Required | Description |
|---|---|---|---|
| `path` | string | yes | Project directory. Must exist and contain a `Cargo.toml`. |
| `package` | string | no | Workspace member to run on (`--package`). |
| `features` | string[] | no | Features to enable (`--features`). |
| `timeout_secs` | integer | no | Override the default timeout for this call. |

### `cargo_check` and `cargo_clippy`

Run `cargo check` / `cargo clippy` with `--message-format=json` and return:

```json
{
  "success": true,
  "error_count": 0,
  "warning_count": 1,
  "diagnostics": [
    {
      "level": "warning",
      "code": "clippy::needless_return",
      "message": "unneeded `return` statement",
      "file": "src/lib.rs",
      "line_start": 6,
      "column_start": 5,
      "suggestion": {
        "message": "remove `return`",
        "replacement": "a + b",
        "file": "src/lib.rs",
        "line_start": 6,
        "column_start": 5,
        "line_end": 6,
        "column_end": 17
      }
    }
  ]
}
```

- `level` is one of `error`, `warning`, `note`, `help`, `failure-note`, `ice`.
- `code` is a rustc error code (`E0308`) or lint name (`unused_variables`,
  `clippy::…`), or `null`.
- `file` is relative to the workspace root, as cargo reports it. For errors
  raised inside a macro, the location points at the macro call in your code.
- `suggestion` is present only when the compiler proposes a concrete
  replacement; it is `null` otherwise.

### `cargo_test`

Runs `cargo test --message-format=json --no-fail-fast`, so every test binary
runs even if an earlier one fails. Compiler diagnostics are reported as above;
test results are parsed from libtest's text output (stable Rust has no JSON
format for them):

```json
{
  "success": false,
  "passed": 3,
  "failed": 1,
  "ignored": 1,
  "failures": [
    {
      "name": "tests::breaks",
      "output": "thread 'tests::breaks' panicked at src/lib.rs:11:45:\nassertion `left == right` failed\n  left: 2\n right: 3"
    }
  ],
  "error_count": 0,
  "warning_count": 1,
  "diagnostics": [ ... ]
}
```

Doctests are counted too. If the build fails, no tests run: `passed`, `failed`
and `ignored` are 0 and the reason is in `diagnostics`.

### Extra fields and errors

- `truncated: true` appears when more than 200 diagnostics were emitted and
  only the first 200 are returned. Each failing test's output is capped at 8 KB.
- `stderr` (the last 4 KB of cargo's stderr) appears when the run failed but no
  diagnostic or test failure explains why, e.g. an unknown package name or a
  missing `clippy`.
- An invalid `path`, an invalid package or feature name, or a timeout makes the
  tool call itself fail (`isError: true`) with a plain-text message.

## Security

Running `cargo check`, `clippy` or `test` executes the project's build scripts
and procedural macros, and `cargo test` runs its tests. Anything an assistant
points this server at runs with your user's permissions, so only use it on code
you trust.

As a safeguard against argument injection, `package` and `features` values that
start with `-` or contain unexpected characters are rejected before cargo is
called.

## Development

```sh
cargo test     # unit tests for the output parsers and argument validation
cargo clippy --all-targets
```

| File | Purpose |
|---|---|
| `src/main.rs` | Entry point, timeout configuration, stdio transport |
| `src/server.rs` | Tool definitions, argument validation, cargo command line |
| `src/runner.rs` | Spawns cargo in its own process group and enforces the timeout |
| `src/diagnostics.rs` | Parses cargo's JSON messages into diagnostics |
| `src/test_output.rs` | Parses libtest's text output into test results |
