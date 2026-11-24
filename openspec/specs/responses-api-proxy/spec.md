# Responses API Proxy Specification

## Purpose

Define the local OpenAI-compatible Responses API endpoint and the command-line behavior used to start it.

## Requirements

### Requirement: Select the proxy command and bind address

The executable SHALL run the local proxy when invoked with no subcommand, with the `proxy` subcommand, or with proxy options directly.

#### Scenario: Start with defaults

- **WHEN** the executable is invoked without arguments or as `claudex-proxy proxy`
- **THEN** it SHALL bind the proxy to `127.0.0.1:8000`

#### Scenario: Override the bind address

- **WHEN** the proxy is started with `--host <host>` and/or `--port <port>`
- **THEN** it SHALL bind using the supplied values and defaults for any value not supplied

#### Scenario: Reject invalid command-line input

- **WHEN** a flag lacks its value, a port is not a valid unsigned 16-bit integer, or an argument or subcommand is unrecognized
- **THEN** the executable SHALL print a fatal error and exit unsuccessfully

### Requirement: Expose only the Responses endpoint

The proxy SHALL accept only `POST /v1/responses`.

#### Scenario: A different path is requested

- **WHEN** a request targets any other path
- **THEN** the proxy SHALL return status `404` with a JSON error response

#### Scenario: A different method is used

- **WHEN** a request targets `/v1/responses` with a method other than `POST`
- **THEN** the proxy SHALL return status `405` with a JSON error response

#### Scenario: The body is not valid JSON

- **WHEN** a request to the supported endpoint has an invalid JSON body
- **THEN** the proxy SHALL return status `400` with a JSON error response

### Requirement: Forward valid requests to the selected upstream

The proxy SHALL forward each valid request to the selected credential's upstream `/responses` endpoint using locally loaded credentials.

#### Scenario: Forward a non-streaming request

- **WHEN** the request omits `stream` or does not set it to boolean `true`
- **THEN** the proxy SHALL request an `application/json` response upstream

#### Scenario: Forward a streaming request

- **WHEN** the request sets `stream` to boolean `true`
- **THEN** the proxy SHALL request `text/event-stream` upstream
- **AND** it SHALL return the response to the client with content type `text/event-stream`

#### Scenario: Handle requests concurrently

- **WHEN** multiple local requests arrive
- **THEN** the proxy SHALL handle each request on its own thread

### Requirement: Remove the unsupported output-token limit

The proxy SHALL remove a top-level `max_output_tokens` member before forwarding a JSON object to the upstream service.

#### Scenario: The request contains max_output_tokens

- **WHEN** a JSON object contains a top-level `max_output_tokens` member
- **THEN** the proxy SHALL omit that member from the forwarded body
- **AND** it SHALL preserve the object's other members and values

#### Scenario: No normalization is required

- **WHEN** the JSON body is not an object or does not contain a top-level `max_output_tokens` member
- **THEN** the proxy SHALL forward the original request bytes unchanged

### Requirement: Relay upstream responses

The proxy SHALL relay upstream status codes and response bodies to the local client.

#### Scenario: The upstream request succeeds

- **WHEN** the upstream returns a response
- **THEN** the proxy SHALL return its status and body
- **AND** for a non-streaming request it SHALL return the upstream content type

#### Scenario: The upstream returns an HTTP error

- **WHEN** the upstream returns an HTTP error after any eligible credential refresh retry
- **THEN** the proxy SHALL pass through the upstream status and body
- **AND** it SHALL apply the same streaming or non-streaming content-type behavior as a successful response

#### Scenario: Credentials cannot be loaded

- **WHEN** the proxy cannot load credentials for a request
- **THEN** it SHALL return status `502` with a JSON error response

#### Scenario: The upstream transport fails

- **WHEN** the proxy cannot obtain an HTTP response from the upstream service
- **THEN** it SHALL return status `502` with a JSON error response

### Requirement: Trust only local reachability

The proxy SHALL NOT require client authentication and SHALL NOT forward a local client's authorization header upstream.

#### Scenario: A client supplies any authorization value

- **WHEN** a local request includes or omits an authorization header
- **THEN** the proxy SHALL authenticate upstream using credentials loaded from the Codex auth file
