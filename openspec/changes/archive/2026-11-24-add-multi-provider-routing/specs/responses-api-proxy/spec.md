# Spec Delta

## ADDED Requirements

### Requirement: Preserve Responses semantics across providers
The proxy SHALL preserve Responses input, output items, tools, tool results, reasoning data, usage, completion states, and errors across provider translation.

#### Scenario: Codex serves the Responses request
- **WHEN** the resolved model selects Codex
- **THEN** the proxy SHALL produce a provider-native Responses request
- **AND** the proxy SHALL return a Responses-compatible result

#### Scenario: Anthropic serves the Responses request
- **WHEN** the resolved model selects Anthropic
- **THEN** the proxy SHALL translate the request into a provider-native Messages request
- **AND** the proxy SHALL translate the result into Responses-compatible output items, usage, completion, and error semantics

#### Scenario: A requested feature has no safe translation
- **WHEN** the selected provider cannot represent a requested Responses feature without losing required meaning
- **THEN** the proxy SHALL reject the request with an explanatory compatibility error
- **AND** the proxy SHALL NOT silently remove or alter that feature

### Requirement: Return optional proxy session metadata
The proxy SHALL return safe routing and session metadata only when the client explicitly requests it.

#### Scenario: Metadata is requested
- **WHEN** a client opts into proxy metadata
- **THEN** the response SHALL identify the proxy session, selected provider, requested model, resolved upstream model, and upstream request identifier when available
- **AND** the metadata SHALL NOT contain credentials, refresh tokens, account identifiers, or raw authorization headers

#### Scenario: Metadata is not requested
- **WHEN** a client does not opt into proxy metadata
- **THEN** the response body and event stream SHALL remain compatible with the selected inbound protocol without proxy-specific metadata events or fields

#### Scenario: A streaming response includes metadata
- **WHEN** metadata is requested for a streaming response
- **THEN** the proxy SHALL emit the metadata in a protocol-safe event before translated model output events

## MODIFIED Requirements

### Requirement: Expose only the Responses endpoint

The proxy SHALL accept only `POST /v1/responses` and `POST /v1/messages` as public inference routes.

#### Scenario: A different path is requested

- **WHEN** a request targets any path other than `/v1/responses` or `/v1/messages`
- **THEN** the proxy SHALL return status `404` with a protocol-appropriate JSON error response

#### Scenario: A different method is used

- **WHEN** a request targets `/v1/responses` or `/v1/messages` with a method other than `POST`
- **THEN** the proxy SHALL return status `405` with a protocol-appropriate JSON error response

#### Scenario: The body is not valid JSON

- **WHEN** a request to a supported endpoint has an invalid JSON body
- **THEN** the proxy SHALL return status `400` with a protocol-appropriate JSON error response

### Requirement: Forward valid requests to the selected upstream

The proxy SHALL resolve the requested model and send each valid request to that model's configured upstream provider using locally loaded provider credentials.

#### Scenario: Forward a non-streaming request

- **WHEN** the request omits `stream` or does not set it to boolean `true`
- **THEN** the proxy SHALL request a complete provider-native response
- **AND** it SHALL return a complete response in the inbound client protocol

#### Scenario: Forward a streaming request

- **WHEN** the request sets `stream` to boolean `true`
- **THEN** the proxy SHALL request provider-native streaming
- **AND** it SHALL return a translated event stream with content type `text/event-stream`

#### Scenario: Handle requests concurrently

- **WHEN** multiple local requests arrive
- **THEN** the proxy SHALL handle each request independently without sharing request-specific credentials, translation state, or session metadata

### Requirement: Remove the unsupported output-token limit

The proxy SHALL normalize the inbound output-token limit according to the provider selected by the requested model.

#### Scenario: The request contains max_output_tokens

- **WHEN** a Responses request contains a top-level `max_output_tokens` member
- **THEN** the proxy SHALL omit it when the resolved provider is Codex
- **AND** the proxy SHALL map it to the provider-native output limit when the resolved provider is Anthropic
- **AND** it SHALL preserve the request's other supported members and values

#### Scenario: Anthropic requires an output-token limit

- **WHEN** a Responses request is routed to Anthropic
- **THEN** the proxy SHALL map the requested output-token limit to the provider-native field
- **AND** it SHALL apply a configured default when the inbound request omits a required provider-native limit

#### Scenario: No normalization is required

- **WHEN** a Responses request routed to Codex does not contain a top-level `max_output_tokens` member
- **THEN** the proxy SHALL preserve the request fields except for other explicitly required protocol translations

### Requirement: Relay upstream responses

The proxy SHALL return upstream outcomes in the protocol used by the local client, translating provider-native bodies and events when the protocols differ.

#### Scenario: The upstream request succeeds

- **WHEN** the upstream returns a successful response
- **THEN** the proxy SHALL return the corresponding success status and protocol-compatible body or event stream

#### Scenario: The upstream returns an HTTP error

- **WHEN** the upstream returns an HTTP error after any eligible credential refresh retry
- **THEN** the proxy SHALL return a protocol-compatible error that preserves the upstream status and safe error details

#### Scenario: Credentials cannot be loaded

- **WHEN** the proxy cannot load credentials for the selected provider
- **THEN** it SHALL return status `502` with a protocol-compatible JSON error response

#### Scenario: The upstream transport fails

- **WHEN** the proxy cannot obtain an HTTP response from the selected provider
- **THEN** it SHALL return status `502` with a protocol-compatible JSON error response

### Requirement: Trust only local reachability

The proxy SHALL NOT require client authentication and SHALL NOT forward a local client's authorization or API key headers upstream.

#### Scenario: A client supplies any authorization value

- **WHEN** a local request includes or omits authorization or API key headers
- **THEN** the proxy SHALL authenticate upstream using only credentials resolved for the provider selected by the requested model
