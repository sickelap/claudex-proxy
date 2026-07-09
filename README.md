# codex-chat

`codex-chat` is a small, dependency-free Python bridge for using credentials from the Codex CLI with the OpenAI Responses API. It can connect agentic coding tools that support the OpenAI Responses API to the ChatGPT Codex backend. It provides two ways to send requests:

- `chat.py`: an interactive terminal chat client.
- `openai_api_proxy.py`: a local OpenAI-compatible Responses API endpoint for agentic coding tools and other API clients.

Both scripts read `~/.codex/auth.json`. Codex OAuth credentials are sent to the ChatGPT Codex backend; an `OPENAI_API_KEY` entry in that file is used with the public OpenAI API instead. OAuth credentials are refreshed in memory after a `401` or `403` response when a refresh token is available.

## Requirements

- Python 3.8 or newer
- A Codex CLI login, which creates `~/.codex/auth.json`
- An agentic coding tool or API client that supports the OpenAI Responses API, if you want to use the proxy

There are no third-party Python dependencies.

Log in with Codex before running either script:

```sh
codex login
```

## Interactive Chat

Run the terminal client from the repository root:

```sh
python3 chat.py
```

Enter prompts at the `>` prompt. The client keeps the current conversation in memory and streams the response text. Use `/exit`, `/quit`, or Ctrl-D to quit.

The default model is `gpt-5.5`. Override it with `OPENAI_MODEL`:

```sh
OPENAI_MODEL=gpt-5.6-sol python3 chat.py
```

Conversation history is not persisted.

## OpenAI API Proxy

The proxy exposes `POST /v1/responses` on `127.0.0.1:8000` and forwards requests to the upstream Responses API. It supports regular JSON responses and streaming server-sent events, so it can be used by agentic coding tools that allow a custom OpenAI base URL.

Start it in one terminal:

```sh
python3 openai_api_proxy.py
```

Use a different local address or port when needed:

```sh
python3 openai_api_proxy.py --host 127.0.0.1 --port 9000
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

- `codex_auth.py`: loads Codex OAuth credentials or an OpenAI API key and handles token refresh.
- `chat.py`: maintains an in-memory conversation and renders streamed response text.
- `openai_api_proxy.py`: forwards local Responses API requests and streams upstream responses unchanged.
- `opencode.json.sample`: example OpenCode provider and model configuration.
- `tests/`: standard-library unit and proxy integration tests.

## Tests

Run the test suite with:

```sh
python3 -m unittest discover -s tests -v
```

## License

MIT. See [LICENSE](LICENSE).
