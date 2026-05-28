# ccs-proxy

`ccs-proxy` is a local HTTP and WebSocket relay for Codex remote-control traffic.

Codex App allows remote-control connections to `localhost`, so this proxy lets Codex App talk to a local address while forwarding traffic to a configurable upstream proxy.

Use `http://localhost:8000` in Codex App configuration. Do not use `127.0.0.1` or another port for Codex App URLs unless you know the App version you are using still attaches ChatGPT authentication headers for that host. Current Codex App builds only auto-attach those headers for a small allowlist that includes `localhost:8000`.

## Build

Debug build:

```bash
cargo build
```

Release build:

```bash
cargo build --release
```

The compiled binary is:

- macOS/Linux: `target/release/ccs-proxy`
- Windows: `target\release\ccs-proxy.exe`

## Install

```bash
cargo install --path .
```

## Run

Default upstream:

```bash
ccs-proxy
```

Custom upstream:

```bash
CCS_PROXY_UPSTREAM_BASE_URL=https://your-proxy.example ccs-proxy
```

Windows PowerShell:

```powershell
$env:CCS_PROXY_UPSTREAM_BASE_URL = "https://your-proxy.example"
.\ccs-proxy.exe
```

## Configuration

Environment variables and CLI flags are both supported. CLI flags take precedence.

| Environment variable | CLI flag | Default |
| --- | --- | --- |
| `CCS_PROXY_LISTEN` | `--listen` | `127.0.0.1:8000` |
| `CCS_PROXY_UPSTREAM_BASE_URL` | `--upstream-base-url` | `https://chatgpt.claudecode.store` |
| `CCS_PROXY_UPSTREAM_PREFIX` | `--upstream-prefix` | empty |
| `RUST_LOG` | n/a | `info` |

## Path Mapping

`ccs-proxy` builds the upstream URL like this:

```text
{CCS_PROXY_UPSTREAM_BASE_URL}{CCS_PROXY_UPSTREAM_PREFIX}{incoming request path and query}
```

For Codex remote-control compatibility, this one path shape is normalized before forwarding:

```text
.../backend-api/codex/wham/remote/control/*
-> .../backend-api/wham/remote/control/*
```

Codex App can produce the first form when `chatgpt_base_url` ends in `/backend-api/codex` and the UI requests `/wham/remote/control/*`. Many upstream proxies expose the remote-control server under `/backend-api/wham/remote/control/*`.

`CCS_PROXY_UPSTREAM_PREFIX` is optional. It is useful when your upstream service requires a fixed Codex backend base path, but you do not want to repeat that path in every Codex local URL.

This prefix is for routing only. Do not put a user id in the path just to identify the authenticated user. Codex App sends authentication headers to the local proxy, and `ccs-proxy` forwards end-to-end headers such as `Authorization` to the upstream service.

Authentication and routing are intentionally separate:

- Authentication comes from the `Authorization` header sent by Codex App.
- Routing comes from the request path plus the optional `CCS_PROXY_UPSTREAM_PREFIX`.
- `ccs-proxy` does not require, parse, or inject a user id in the URL path.

Choose one of these two styles.

### Style 1: Full Path In Codex Config

Leave `CCS_PROXY_UPSTREAM_PREFIX` empty:

```bash
CCS_PROXY_UPSTREAM_BASE_URL=https://your-proxy.example ccs-proxy
```

Then put the full upstream Codex backend path in Codex config, but replace the upstream host with `localhost:8000`:

```toml
chatgpt_base_url = "http://localhost:8000/<upstream-codex-base-path>"
```

Forwarding example:

```text
http://localhost:8000/<upstream-codex-base-path>/wham/remote/control/server
-> https://your-proxy.example/<upstream-codex-base-path>/wham/remote/control/server
```

### Style 2: Fixed Path In `CCS_PROXY_UPSTREAM_PREFIX`

Put the fixed upstream Codex backend path in `CCS_PROXY_UPSTREAM_PREFIX`:

```bash
CCS_PROXY_UPSTREAM_BASE_URL=https://your-proxy.example \
CCS_PROXY_UPSTREAM_PREFIX=/<upstream-codex-base-path> \
ccs-proxy
```

Then Codex config can use only the local proxy origin:

```toml
chatgpt_base_url = "http://localhost:8000"
```

Forwarding example:

```text
http://localhost:8000/wham/remote/control/server
-> https://your-proxy.example/<upstream-codex-base-path>/wham/remote/control/server
```

Do not use both styles at the same time. If you put the full path in Codex config and also set `CCS_PROXY_UPSTREAM_PREFIX`, the path will be duplicated.

## Codex App

Use a local URL in `~/.codex/config.toml`. The path, if present, must be the Codex backend base path required by your upstream service:

```toml
chatgpt_base_url = "http://localhost:8000/<upstream-codex-base-path>"
```

If you set `CCS_PROXY_UPSTREAM_PREFIX`, use this instead:

```toml
chatgpt_base_url = "http://localhost:8000"
```

Do not copy placeholders literally. `ccs-proxy` only relays traffic; it does not know the account, tenant, or routing path required by a specific upstream provider.

The hostname matters. `localhost:8000` is the safe default because Codex App will attach its ChatGPT `Authorization` header for this host. `127.0.0.1:8000`, `localhost:8787`, and `127.0.0.1:8787` can make some remote-control UI requests arrive at the upstream without `Authorization`.

Then start Codex App normally. The proxy forwards HTTP and WebSocket traffic to the configured upstream.

## Notes

- This project only relays traffic.
- It forwards `Authorization` and other end-to-end request headers, but does not create, refresh, or validate access tokens.
- It does not manage platform accounts, account IDs, or desktop tokens.
- The upstream proxy must implement the required Codex and ChatGPT remote-control behavior.
