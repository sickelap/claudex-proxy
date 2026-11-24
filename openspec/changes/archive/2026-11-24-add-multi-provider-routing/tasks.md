# Tasks

## 1. Provider-neutral core and model registry

- [x] 1.1 Add typed domain models for requests, structured content blocks, tools, reasoning, usage, errors, and ordered inference events; verify unit tests cover construction and serialization without flattening structured content.
- [x] 1.2 Add the `ModelRegistry`, `InferenceProvider`, `CredentialProvider`, and session-id ports plus the inference orchestration service; verify fake-adapter tests cover provider selection, unknown models, and independent concurrent request state.
- [x] 1.3 Implement JSON model-registry configuration through `--config` and `CLAUDEX_CONFIG`, including conservative built-in mappings; verify tests cover valid mappings, duplicate ids, invalid providers, missing files, and precedence.
- [x] 1.4 Document the model registry and add a checked-in example configuration; verify every example model resolves in a configuration-loading test.

## 2. Existing Codex behavior as adapters

- [x] 2.1 Split Codex/OpenAI credential loading and provider-tagged credentials out of the current combined `Auth`; verify the existing auth-file, account-id, client-id, API-key fallback, and failure tests continue to pass.
- [x] 2.2 Implement the Codex Responses provider adapter with existing endpoint, headers, output-token normalization, and one-time OAuth refresh; verify mocked transport tests cover success, `401`/`403` replay, non-refreshable errors, and request header isolation.
- [x] 2.3 Route `POST /v1/responses` through the application service for Codex mappings while preserving current JSON and SSE behavior; verify all existing proxy contract tests pass through the new boundary.
- [x] 2.4 Route terminal chat through the same application service without changing its visible history and failure behavior; verify the interactive-chat unit tests pass with a fake provider.

## 3. Anthropic credentials and native provider adapter

- [x] 3.1 Implement Anthropic credential resolution with `ANTHROPIC_API_KEY` before `CLAUDE_CODE_OAUTH_TOKEN`; verify tests cover precedence, empty values, missing credentials, provider tagging, and absence of Codex credentials in Anthropic headers.
- [x] 3.2 Implement non-streaming Anthropic Messages requests with the correct endpoint, API-key or Bearer header, API version, content type, and upstream model; verify mocked transport tests assert exact headers and safe error handling for both credential kinds.
- [x] 3.3 Implement Anthropic SSE decoding into ordered canonical events for text, tools, reasoning, signatures, usage, completion, ping, unknown events, and stream errors; verify golden stream fixtures cover each event class and event ordering.

## 4. Messages-compatible inbound API

- [x] 4.1 Add `POST /v1/messages` routing, JSON validation, method/path handling, and Messages-compatible errors; verify HTTP contract tests cover valid input, malformed JSON, missing required fields, unsupported methods, and unsupported paths.
- [x] 4.2 Implement the native Messages-to-Anthropic fast path while stripping local client credentials and proxy-only fields; verify non-streaming and streaming tests preserve structured blocks, tool deltas, reasoning data, usage, and stop reasons.
- [x] 4.3 Add an Anthropic-compatible coding-harness example using the proxy base URL; verify the documented request shape passes the Messages input adapter tests.

## 5. Cross-protocol translation

- [x] 5.1 Implement Responses-to-Messages request translation for conversation content, system instructions, tools, tool results, tool choice, reasoning controls, output limits, and provider options; verify table-driven fixtures cover every supported mapping and reject lossy inputs.
- [x] 5.2 Implement Messages-to-Responses request translation for the same capability set; verify table-driven fixtures cover every supported mapping and reject lossy inputs.
- [x] 5.3 Implement provider-response translation for content blocks, tool calls, reasoning, usage, stop/completion states, HTTP errors, and transport errors in both directions; verify golden non-streaming fixtures preserve observable semantics.
- [x] 5.4 Implement Responses-to-Messages and Messages-to-Responses SSE encoders using the canonical event state machine; verify accumulated translated streams equal their corresponding non-streaming results and invalid event sequences fail deterministically.
- [x] 5.5 Enforce the capability matrix and namespaced provider-option validation before upstream calls; verify tests prove mismatched providers, unknown options, and unsupported translations do not contact a fake provider.

## 6. Session metadata and user-facing configuration

- [x] 6.1 Parse and remove the proxy metadata extension, accept or generate an opaque session id, and collect safe routing context; verify tests prove proxy fields never reach upstream payloads and sensitive credential/account data cannot enter metadata.
- [x] 6.2 Add opt-in metadata to non-streaming responses and as the first namespaced streaming event for both client protocols; verify strict compatibility fixtures remain unchanged when metadata is not requested.
- [x] 6.3 Update the README and sample harness configurations for both standard routes, model mappings, supported credential sources, metadata, and known cross-provider limits; verify all documented JSON examples parse and all referenced sample files exist.

## 7. Integration verification

- [x] 7.1 Add an end-to-end mocked-upstream matrix covering Responses-to-Codex, Responses-to-Anthropic, Messages-to-Anthropic, and Messages-to-Codex in streaming and non-streaming modes; verify every matrix case routes by model and returns the inbound protocol.
- [x] 7.2 Run `cargo fmt --check`, `cargo test`, and strict Clippy across all targets; verify every command completes successfully with no warnings introduced by the change.
