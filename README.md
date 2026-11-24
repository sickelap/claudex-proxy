# claudex-proxy

`claudex-proxy` is a small, self-contained Rust binary for using credentials from the Codex CLI with the OpenAI Responses API. It can connect agentic coding tools that support the OpenAI Responses API to the ChatGPT Codex backend. It provides two ways to send requests, selected by subcommand:

- `claudex-proxy chat`: an interactive terminal chat client.
- `claudex-proxy` / `claudex-proxy proxy`: a local OpenAI-compatible Responses API endpoint for agentic coding tools and other API clients.

Both subcommands read `~/.codex/auth.json`. Codex OAuth credentials are sent to the ChatGPT Codex backend; an `OPENAI_API_KEY` entry in that file is used with the public OpenAI API instead. OAuth credentials are refreshed in memory after a `401` or `403` response when a refresh token is available.

## Requirements

- The Rust toolchain (`cargo`, `rustc`) to build the binary
- A Codex CLI login, which creates `~/.codex/auth.json`
- An agentic coding tool or API client that supports the OpenAI Responses API, if you want to use the proxy

Log in with Codex before running either subcommand:

```sh
codex login
```

## Building

```sh
cargo build --release
```

The binary is written to `target/release/claudex-proxy`.

## Interactive Chat

Run the terminal client from the repository root:

```sh
claudex-proxy chat
```

Enter prompts at the `>` prompt. The client keeps the current conversation in memory and streams the response text. Use `/exit`, `/quit`, or Ctrl-D to quit.

The default model is `gpt-5.5`. Override it with `OPENAI_MODEL`:

```sh
OPENAI_MODEL=gpt-5.6-sol claudex-proxy chat
```

Conversation history is not persisted.

## OpenAI API Proxy

The proxy exposes `POST /v1/responses` on `127.0.0.1:8000` and forwards requests to the upstream Responses API. It supports regular JSON responses and streaming server-sent events, so it can be used by agentic coding tools that allow a custom OpenAI base URL.

Start it in one terminal (`proxy` is the default subcommand):

```sh
claudex-proxy
```

Use a different local address or port when needed:

```sh
claudex-proxy proxy --host 127.0.0.1 --port 9000
```

Configure your tool to use this OpenAI API base URL:

```text
http://127.0.0.1:8000/v1
```

The API key can be any non-empty placeholder if the client requires one. The proxy ignores incoming authorization headers and authenticates upstream requests with `~/.codex/auth.json`.

### OpenCode Example

For OpenCode, copy the included sample configuration into this project's OpenCode configuration and start OpenCode in another terminal:

```sh
cp opencode.json.sample ~/.config/opencode/opencode.json
opencode
```

If the proxy uses a different port, update `provider.openai-api-local.options.baseURL` in `opencode.json`. The sample defines several model and reasoning-effort variants and defaults to `openai-api-local/gpt-5.6-sol`. Its configured `apiKey` is only a placeholder required by the OpenAI SDK.

You can also call the proxy directly:

```sh
curl http://127.0.0.1:8000/v1/responses \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-5.5","input":"Hello","store":false}'
```

### Compatibility

This is not a complete implementation of the OpenAI API. It only accepts `POST /v1/responses`; tools that require `/v1/chat/completions` or other endpoints are not supported. Requests and responses are otherwise passed through, except that `max_output_tokens` is removed for compatibility with the Codex backend.

### Security

The proxy has no client authentication. Keep it bound to the default `127.0.0.1`; do not expose it to a network or the public internet. Anyone able to reach the proxy can make requests using your upstream account.

## Files

- `src/auth.rs`: loads Codex OAuth credentials or an OpenAI API key and handles token refresh.
- `src/chat.rs`: maintains an in-memory conversation and renders streamed response text.
- `src/proxy.rs`: forwards local Responses API requests and streams upstream responses unchanged.
- `src/main.rs`: CLI argument parsing and subcommand dispatch.
- `opencode.json.sample`: example OpenCode provider and model configuration.

## Tests

Run the test suite with:

```sh
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
