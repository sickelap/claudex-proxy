# Proposal

## Why

`claudex-proxy` currently exposes only the OpenAI Responses protocol and couples request handling, Codex credentials, and upstream transport. Supporting coding harnesses across OpenAI/Codex and Anthropic/Claude requires one gateway that preserves their standard client protocols while routing models to the correct provider behind a stable application boundary.

## What Changes

- Keep one proxy base URL while supporting both standard `POST /v1/responses` and `POST /v1/messages` compatibility routes.
- Add an explicit model registry that selects the upstream provider and concrete upstream model; clients select a model but do not select a provider directly.
- Translate requests, streaming events, tool calls, reasoning content, usage, completion states, and errors between the inbound client protocol and the selected provider protocol.
- Add Anthropic credential support using documented Claude Code-compatible credential sources, including `CLAUDE_CODE_OAUTH_TOKEN` and `ANTHROPIC_API_KEY`.
- Add opt-in, protocol-safe proxy session metadata without exposing credentials or account identifiers.
- Restructure the application around ports and adapters so HTTP/chat inputs, routing, credentials, and provider transports are independently testable.
- Preserve provider-specific features through validated, namespaced options when the inbound protocol cannot express them directly.

## Capabilities

### New Capabilities

- `model-provider-routing`: Resolve a public model identifier to an allowed provider and upstream model, then execute the request through that provider.
- `messages-api-proxy`: Accept Anthropic Messages-compatible requests and responses at the proxy base URL, including streaming and cross-provider translation.

### Modified Capabilities

- `credential-authentication`: Load and apply provider-specific Codex/OpenAI and Claude/Anthropic credentials without leaking them across adapters.
- `responses-api-proxy`: Route Responses requests by model, translate them for Anthropic when required, and optionally return safe proxy session metadata.

## Impact

- Refactors `src/auth.rs`, `src/proxy.rs`, `src/chat.rs`, and command wiring in `src/main.rs` around provider-neutral application ports.
- Adds model registry/configuration, provider adapters, protocol codecs, and provider-specific credential loaders.
- Expands the public HTTP surface with `/v1/messages` while preserving `/v1/responses` and the existing proxy base address.
- Requires compatibility and translation tests for JSON and SSE flows in both protocol directions, including tools and reasoning blocks.
- Documentation and sample harness configuration will need to describe model mappings, supported credentials, metadata, and compatibility limits.
