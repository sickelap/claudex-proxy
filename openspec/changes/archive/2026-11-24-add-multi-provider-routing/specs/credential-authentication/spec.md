# Spec Delta

## ADDED Requirements

### Requirement: Load Claude-compatible Anthropic credentials
The system SHALL resolve Anthropic credentials from documented credential sources that can also be used by Claude Code.

#### Scenario: An Anthropic API key is available
- **WHEN** `ANTHROPIC_API_KEY` contains a non-empty value
- **THEN** the system SHALL use that value as the Anthropic API key

#### Scenario: A Claude Code OAuth token is available
- **WHEN** no non-empty `ANTHROPIC_API_KEY` is available
- **AND** `CLAUDE_CODE_OAUTH_TOKEN` contains a non-empty value
- **THEN** the system SHALL use that token as the Anthropic OAuth credential

#### Scenario: No Anthropic credential is available
- **WHEN** neither supported Anthropic credential source contains a usable value
- **THEN** Anthropic credential loading SHALL fail with an explanatory error
- **AND** credentials for another provider SHALL NOT be used as a fallback

### Requirement: Isolate credentials by provider
The system SHALL make each resolved credential available only to the provider adapter for which it was loaded.

#### Scenario: A request is routed to Codex
- **WHEN** the model mapping selects Codex
- **THEN** no Anthropic credential SHALL be attached to the upstream request

#### Scenario: A request is routed to Anthropic
- **WHEN** the model mapping selects Anthropic
- **THEN** no Codex or OpenAI credential SHALL be attached to the upstream request

## MODIFIED Requirements

### Requirement: Authenticate upstream requests

The system SHALL authenticate each upstream request with the selected provider's locally resolved credential rather than an authorization header supplied by a local proxy client.

#### Scenario: Send a standard authenticated request

- **WHEN** the system sends a request through the Codex provider adapter
- **THEN** it SHALL include the selected credential as a Bearer token
- **AND** it SHALL declare a JSON content type
- **AND** it SHALL include its configured user agent

#### Scenario: Send a request for a ChatGPT account

- **WHEN** a ChatGPT account identifier is available for a Codex request
- **THEN** it SHALL include it in the `ChatGPT-Account-ID` header

#### Scenario: Send an Anthropic request with an API key

- **WHEN** the system sends an Anthropic request using `ANTHROPIC_API_KEY`
- **THEN** it SHALL put the credential in the Anthropic API key header
- **AND** it SHALL include the required Anthropic API version and JSON content headers

#### Scenario: Send an Anthropic request with a Claude Code OAuth token

- **WHEN** the system sends an Anthropic request using `CLAUDE_CODE_OAUTH_TOKEN`
- **THEN** it SHALL put the credential in the Bearer authorization header
- **AND** it SHALL include the required Anthropic API version and JSON content headers

#### Scenario: A local client supplies authorization

- **WHEN** a local proxy client supplies any authorization or API key header
- **THEN** the system SHALL ignore that credential for upstream authentication
