# Design

## Context

See `proposal.md` for motivation. The current binary has a small synchronous HTTP stack and directly combines HTTP parsing, credential loading, upstream selection, request normalization, retry behavior, and response relay. `Auth` contains both credential and routing data, while the chat and proxy paths independently construct Responses requests. Streaming chat reduces structured events to text, which cannot preserve tools, reasoning, or provider-specific content.

The public compatibility constraint is stronger than a single custom route: coding harnesses generally append their provider's standard path and emit that provider's schema. The gateway therefore needs one base address with two inbound protocol adapters while keeping provider selection behind the model mapping.

## Goals / Non-Goals

**Goals:**

- Isolate domain and application behavior from HTTP, credential storage, and provider wire formats.
- Preserve standard Responses and Messages client compatibility at one proxy base address.
- Make the model registry the only authority that selects an upstream provider.
- Preserve structured tools, reasoning, usage, errors, and streaming order across translations.
- Maximize native feature parity and reject cross-protocol features that cannot be represented safely.
- Keep credential resolution and refresh behavior provider-specific.

**Non-Goals:**

- Reproduce undocumented Codex or Claude Code system prompts, telemetry, private headers, or version-specific implementation details.
- Read Claude Code's macOS Keychain entry or private `.credentials.json` representation.
- Add durable conversation storage; session metadata is correlation data, not server-side conversation state.
- Support Chat Completions or provider endpoints other than Responses and Messages in this change.
- Guarantee that every feature of one provider has a meaningful representation in the other provider's protocol.

## Decisions

### Use a ports-and-adapters application boundary

Introduce a provider-neutral application service that receives a normalized inference request, resolves its model, loads the selected provider credential, invokes a provider port, and returns normalized events. HTTP Responses, HTTP Messages, and terminal chat are inbound adapters. Codex and Anthropic transports, credential sources, and configuration are outbound adapters.

The main ports are:

- `ModelRegistry`: resolve a public model identifier to provider, upstream model, and declared capabilities.
- `InferenceProvider`: execute a normalized request and return an event stream.
- `CredentialProvider`: resolve a credential for one provider without exposing another provider's credential.
- `SessionIdGenerator`: create correlation identifiers when metadata is requested and the client did not supply one.

This is preferred over adding conditionals to `proxy.rs` because protocol conversion, authentication, and routing can then be tested independently. Traits are limited to boundaries that need multiple implementations; ordinary domain transformations remain concrete functions.

### Keep two standard inbound routes and one application core

The proxy continues to listen on one address and exposes:

- `POST /v1/responses` through a Responses codec.
- `POST /v1/messages` through a Messages codec.

Both codecs validate their client protocol, remove proxy-only extension fields, and create the same normalized request. The incoming path does not choose the provider. This preserves unmodified harness compatibility while avoiding a custom `/v1/inference` contract.

Alternative considered: expose only `/v1/inference`. This provides a cleaner schema but prevents standard coding harnesses from connecting by base URL alone.

### Route through an explicit model registry

Each registry entry contains a public model id, provider kind, upstream model id, supported capabilities, and optional provider defaults. The registry is created from a JSON configuration selected by `--config` or `CLAUDEX_CONFIG`; a missing explicit configuration uses conservative built-in mappings matching the shipped examples. Invalid entries, duplicate public ids, and unavailable provider kinds fail during startup.

Requests cannot name a provider. Namespaced `provider_options` are validated after model resolution and rejected when they target a different provider or unsupported capability. This prevents a client from bypassing policy through request fields or model-name prefix tricks.

Alternative considered: infer providers from names such as `gpt-*` and `claude-*`. Prefix routing is simple but ambiguous, difficult to alias, and gives request strings unintended policy authority.

### Use a structured canonical request and event model

The domain request represents conversation turns, system instructions, text and media content, tools, tool results, tool choice, output constraints, reasoning controls, streaming intent, provider options, and proxy metadata options. Content is represented as tagged blocks rather than untyped text.

Provider output is normalized into ordered events such as response start, content block start, text delta, reasoning delta, tool argument delta, content block completion, usage update, completion, and error. Non-streaming responses are accumulated from the same event model used for streaming, avoiding separate semantic implementations.

Opaque provider-native blocks may be retained only for a request that returns through the same client protocol and provider. A cross-protocol translation must either map a block explicitly or fail with a compatibility error. It must never discard an unknown block silently.

Alternative considered: use the OpenAI Responses JSON model internally. That reduces initial work but makes Anthropic reasoning signatures, content block ordering, cache controls, and tool semantics secondary extensions rather than first-class data.

### Provide a native fast path and a translated path

When inbound and upstream protocols match, the adapter preserves provider-native fields and stream events after validating the model, stripping local credentials, and applying local credentials. This is the highest-fidelity path.

When protocols differ, the inbound codec produces the canonical request, the provider adapter creates the other provider's native request, and the outbound codec emits the client's expected response. Translation covers tools and tool results, reasoning data that may be safely exposed, usage accounting, stop reasons, errors, and stream ordering. Capability checks occur before the upstream call whenever incompatibility can be determined from the request.

This approach keeps native parity without pretending cross-provider APIs are identical.

### Treat provider identity as supported native behavior, not CLI impersonation

The Codex adapter preserves the existing Codex endpoint, account header, Bearer authentication, user agent, and one-time OAuth refresh behavior. The Anthropic adapter uses the Messages endpoint, required API-version and content headers, and the authentication header appropriate to the resolved credential.

Anthropic credential precedence for the supported subset is `ANTHROPIC_API_KEY`, then `CLAUDE_CODE_OAUTH_TOKEN`, matching Claude Code's documented precedence between those sources. API keys use the Anthropic API-key header; OAuth tokens use Bearer authorization. Claude Code OAuth failures are returned to the client rather than refreshed through an undocumented flow.

The implementation does not spoof private CLI headers or claim byte-for-byte identity with a particular Codex or Claude Code release. The supported contract is that upstreams receive their native protocol and credentials accepted for coding use.

### Make proxy metadata explicit and non-sensitive

Both inbound protocols accept a proxy-only extension containing `include_metadata` and an optional opaque `session_id`. The inbound adapter removes this extension before provider translation. If metadata is requested and no id is supplied, the application creates one for that request.

For non-streaming responses, the outbound codec adds a namespaced proxy metadata object. For streaming responses, it emits one namespaced metadata event before model output events. Metadata contains only the session id, provider, requested model, resolved upstream model, and upstream request id when available. It never includes credentials, account ids, authorization headers, or raw credential errors.

Clients that do not opt in receive the standard provider-compatible schema without proxy extensions. Sessions do not persist conversation state in this change.

### Organize modules by boundary

The target layout separates:

- domain request, content, response, event, error, and capability types;
- application routing and inference orchestration;
- inbound Responses, Messages, and terminal-chat adapters;
- outbound Codex and Anthropic provider adapters;
- Codex and Anthropic credential adapters;
- model-registry configuration.

The current behavior is first wrapped as the Codex implementation of these ports before Anthropic behavior is added. This keeps the refactor testable and avoids combining architectural movement with two new protocol implementations at once.

## Risks / Trade-offs

- [Cross-provider semantics are not one-to-one] -> Maintain a capability matrix, test explicit mappings, and reject unsupported translations before contacting the provider.
- [Streaming translation can produce invalid event order] -> Use one canonical event state machine and golden contract tests for both streaming directions.
- [Thinking signatures or opaque blocks may be required on later turns] -> Preserve them as structured or provider-opaque blocks for same-provider continuation and reject unsafe cross-provider replay.
- [Provider APIs and model names evolve] -> Keep provider codecs and the model registry isolated; avoid provider inference from naming conventions.
- [Extra metadata can break strict SDKs] -> Make it opt-in and remove all proxy fields before upstream calls.
- [Credential leakage across adapters] -> Return provider-tagged credential variants and construct provider headers only inside the matching adapter.
- [The native fast path and translated path may diverge] -> Run shared conformance fixtures against both and accumulate non-streaming output from the streaming event representation.
- [The current synchronous server dedicates a thread to each stream] -> Preserve existing concurrency initially, measure long-lived stream behavior, and defer an async runtime migration unless evidence requires it.

## Migration Plan

1. Add canonical domain types, application ports, and model-registry configuration without changing public behavior.
2. Wrap the existing Codex authentication, refresh, request, and relay behavior in outbound adapters; keep current Responses contract tests passing.
3. Route `/v1/responses` through the application service using default Codex mappings.
4. Add Anthropic credential and Messages provider adapters with mocked transport tests.
5. Add the `/v1/messages` inbound adapter and native Anthropic path.
6. Implement and contract-test cross-protocol request, response, error, and SSE translations.
7. Add opt-in session metadata, configuration examples, compatibility documentation, and harness examples.

Rollback is possible until the new core becomes the only path by retaining the existing Codex adapter behavior and default mapping. If a provider path is not configured, its models remain unavailable without affecting existing Codex mappings.
