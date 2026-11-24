# Spec Delta

## Purpose

Define the Anthropic Messages-compatible proxy route and its behavior when requests are served by either Anthropic or Codex upstream providers.

## ADDED Requirements

### Requirement: Expose an Anthropic Messages-compatible route
The proxy SHALL accept `POST /v1/messages` on the same base address as the Responses route.

#### Scenario: A valid Messages request is submitted
- **WHEN** a client sends a valid Messages request to `POST /v1/messages`
- **THEN** the proxy SHALL resolve its model and process it through the selected provider
- **AND** the proxy SHALL return a Messages-compatible response

#### Scenario: A different method targets the Messages route
- **WHEN** a client uses a method other than `POST` for `/v1/messages`
- **THEN** the proxy SHALL return status `405` with a Messages-compatible error

#### Scenario: The Messages body is invalid
- **WHEN** a client submits invalid JSON or a request that does not satisfy the required Messages fields
- **THEN** the proxy SHALL return status `400` with a Messages-compatible validation error

### Requirement: Preserve Messages semantics across providers
The proxy SHALL preserve Messages content blocks, system instructions, tools, tool results, stop conditions, usage, and errors when normalizing and producing a Messages response.

#### Scenario: Anthropic serves the request
- **WHEN** the resolved model selects Anthropic
- **THEN** the proxy SHALL map the request to the provider-native Messages call without flattening structured content
- **AND** the proxy SHALL return the provider result in Messages-compatible form

#### Scenario: Codex serves the request
- **WHEN** the resolved model selects Codex
- **THEN** the proxy SHALL translate the Messages request into a Responses request
- **AND** the proxy SHALL translate the Responses result into Messages-compatible content, usage, stop, and error semantics

#### Scenario: A requested feature has no safe translation
- **WHEN** the selected provider cannot represent a requested Messages feature without losing required meaning
- **THEN** the proxy SHALL reject the request with an explanatory compatibility error
- **AND** the proxy SHALL NOT silently remove or alter that feature

### Requirement: Stream Messages-compatible events
The proxy SHALL produce the documented Messages server-sent event sequence when a Messages request enables streaming.

#### Scenario: Anthropic stream is relayed
- **WHEN** Anthropic serves a streaming Messages request
- **THEN** the proxy SHALL preserve content block ordering, text deltas, tool input deltas, reasoning data, usage, and completion events

#### Scenario: Codex stream is translated
- **WHEN** Codex serves a streaming Messages request
- **THEN** the proxy SHALL translate Responses events into a valid Messages event sequence
- **AND** the final accumulated stream SHALL represent the same structured result as a non-streaming response

#### Scenario: The upstream stream fails
- **WHEN** the selected upstream reports an error after streaming has begun
- **THEN** the proxy SHALL emit a Messages-compatible error event and terminate the stream

### Requirement: Ignore local client authorization upstream
The Messages route SHALL authenticate upstream requests only with credentials selected by the provider adapter.

#### Scenario: A Messages client supplies authorization
- **WHEN** a local client includes any API key or authorization header
- **THEN** the proxy SHALL NOT forward that credential upstream
- **AND** the selected provider adapter SHALL apply its locally resolved credential
