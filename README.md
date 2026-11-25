# claudex-proxy

`claudex-proxy` is a local gateway for coding tools that speak either the OpenAI Responses API or the Anthropic Messages API. A model registry routes each public model alias to Codex/OpenAI or Anthropic while clients keep using their standard protocol.

- `claudex-proxy chat`: interactive terminal chat.
- `claudex-proxy` / `claudex-proxy proxy`: local `POST /v1/responses` and `POST /v1/messages` endpoints.

## Requirements

- Rust (`cargo` and `rustc`)
- A Codex CLI login for Codex models, or an Anthropic credential for Anthropic models
- A client supporting Responses or Messages

For Codex, log in before starting the proxy:

```sh
codex login
```

## Build and run

```sh
cargo build --release
claudex-proxy
```

The default address is `127.0.0.1:8000`. Change it when needed:

```sh
claudex-proxy proxy --host 127.0.0.1 --port 9000
```

The proxy supports JSON and server-sent event responses on:

- `POST /v1/responses`
- `POST /v1/messages`

OpenAI-compatible clients use `http://127.0.0.1:8000/v1` as their base URL. Anthropic-compatible clients use `http://127.0.0.1:8000`; they append `/v1/messages`.

## Model registry

Clients choose a public model alias. The registry is the only component that selects its provider and upstream model. Built-in mappings cover the models in `examples/opencode.json.sample`. To add Anthropic or custom aliases, copy [`examples/claudex.config.example.json`](examples/claudex.config.example.json):

```sh
claudex-proxy proxy --config ./examples/claudex.config.example.json
```

`CLAUDEX_CONFIG` selects the same file when `--config` is absent; the CLI flag takes precedence. Duplicate aliases, unknown providers, and unreadable or invalid files stop startup. Entries declare capabilities, namespaced provider options, and an optional Anthropic output-token default.

## Credentials

- Codex OAuth or OpenAI API key: `~/.codex/auth.json`
- Anthropic API key: `ANTHROPIC_API_KEY`
- Claude Code OAuth fallback: `CLAUDE_CODE_OAUTH_TOKEN`

`ANTHROPIC_API_KEY` wins when both Anthropic variables are set. Credentials are isolated by provider. Incoming client `Authorization` and API-key headers are never used upstream. Codex OAuth refreshes once in memory after a `401` or `403` when a refresh token is available.

## Client examples

For OpenCode:

```sh
cp examples/opencode.json.sample ~/.config/opencode/opencode.json
opencode
```

For an Anthropic-compatible coding harness, see [`examples/claude-code.env.sample`](examples/claude-code.env.sample). Its placeholder key only satisfies clients that require a value; the proxy does not forward it.

Direct Responses request:

```sh
curl http://127.0.0.1:8000/v1/responses \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-5.6-sol","input":"Hello","store":false}'
```

Direct Messages request:

```sh
curl http://127.0.0.1:8000/v1/messages \
  -H 'Content-Type: application/json' \
  -d @examples/anthropic-messages-request.json
```

## Optional routing metadata

Add this object at the request top level as `"claudex"`:

```json
{
  "include_metadata": true,
  "session_id": "my-opaque-correlation-id"
}
```

The extension is removed before the upstream call. JSON responses receive a `claudex` metadata object. Streams receive `claudex.metadata` as their first event. Metadata contains only the session ID, provider, requested alias, upstream model, and upstream request ID when available.

## Interactive chat

```sh
claudex-proxy chat
```

The chat keeps history in memory and streams text. Use `/exit`, `/quit`, or Ctrl-D to quit. It defaults to `gpt-5.6-sol`; set `OPENAI_MODEL` to another configured alias.

## Compatibility limits

This is not a complete implementation of either provider API. Chat Completions and other routes are not supported. Native protocol/provider pairs are relayed at highest fidelity. Cross-provider paths translate conversation content, system instructions, images, tools, tool results, tool choice, reasoning controls, output limits, usage, stop reasons, and errors.

Features without a safe representation are rejected before contacting the upstream. Signed Anthropic thinking blocks and unknown opaque provider blocks are not replayed across providers.

The proxy has no client authentication. Keep it bound to `127.0.0.1`; anyone who can reach it can use your configured upstream account.

## Files

- `src/application.rs`: model routing, capability checks, credentials, and metadata.
- `src/auth.rs`: provider-specific credential loading and Codex refresh.
- `src/codecs.rs`: Responses/Messages request, response, and SSE translation.
- `src/config.rs`: registry loading and built-in mappings.
- `src/domain.rs`: structured provider-neutral types and ordered events.
- `src/providers.rs`: Codex and Anthropic upstream adapters.
- `src/proxy.rs`: HTTP input/output adapter.
- `src/chat.rs`: terminal input/output adapter.
- `examples/claudex.config.example.json`: model-registry example.
- `examples/opencode.json.sample`: OpenCode example.
- `examples/claude-code.env.sample`: Anthropic-compatible harness example.
- `examples/anthropic-messages-request.json`: Messages request fixture.

## Tests

```sh
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
