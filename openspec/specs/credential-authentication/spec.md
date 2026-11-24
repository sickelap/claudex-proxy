# Credential Authentication Specification

## Purpose

Define how `claudex-proxy` obtains credentials for upstream Responses API calls and refreshes Codex OAuth credentials.

## Requirements

### Requirement: Load credentials from the Codex auth file

The system SHALL load credentials from `$HOME/.codex/auth.json`.

#### Scenario: Codex OAuth credentials are available

- **WHEN** the auth file contains a non-empty `tokens.access_token`
- **THEN** the system SHALL use that access token with the ChatGPT Codex backend
- **AND** the system SHALL retain any refresh token and account identifier found in the token data
- **AND** the system SHALL recover the OAuth client identifier from the access token `client_id` claim or the ID token `aud` claim

#### Scenario: Only an OpenAI API key is available

- **WHEN** no non-empty Codex access token is available
- **AND** the auth file contains a non-empty `OPENAI_API_KEY`
- **THEN** the system SHALL use that key with the public OpenAI API

#### Scenario: No usable credential is available

- **WHEN** the auth file cannot be read, contains invalid JSON, or contains neither a usable Codex access token nor an OpenAI API key
- **THEN** credential loading SHALL fail with an explanatory error

### Requirement: Authenticate upstream requests

The system SHALL authenticate each upstream request with the selected credential rather than an authorization header supplied by a local proxy client.

#### Scenario: Send a standard authenticated request

- **WHEN** the system sends an upstream request
- **THEN** it SHALL include the selected credential as a Bearer token
- **AND** it SHALL declare a JSON content type
- **AND** it SHALL include its configured user agent

#### Scenario: Send a request for a ChatGPT account

- **WHEN** a ChatGPT account identifier is available
- **THEN** the system SHALL include it in the `ChatGPT-Account-ID` header

### Requirement: Refresh rejected OAuth credentials once

The system SHALL attempt a single in-memory OAuth refresh and replay when an upstream request is rejected with status `401` or `403` and a refresh token is available.

#### Scenario: Token refresh succeeds

- **WHEN** an upstream request returns `401` or `403`
- **AND** the OAuth refresh returns a new access token
- **THEN** the system SHALL update its in-memory credential
- **AND** it SHALL replay the upstream request once
- **AND** it SHALL NOT persist the refreshed credentials to disk

#### Scenario: Token refresh is unavailable or fails

- **WHEN** an upstream request returns `401` or `403`
- **AND** no refresh token is available or the refresh attempt fails
- **THEN** the system SHALL return the original upstream rejection without replaying the request

#### Scenario: Upstream returns another error status

- **WHEN** an upstream request returns a status other than `401` or `403`
- **THEN** the system SHALL NOT attempt to refresh the credential
