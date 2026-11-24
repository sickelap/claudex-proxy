# Spec Delta

## Purpose

Define how public model identifiers select an allowed upstream provider and concrete model without exposing provider selection to proxy clients.

## ADDED Requirements

### Requirement: Resolve models through an explicit registry
The system SHALL resolve every requested public model identifier through a configured model registry that names exactly one upstream provider and upstream model.

#### Scenario: A configured model is requested
- **WHEN** a request names a configured public model identifier
- **THEN** the system SHALL select the provider and upstream model recorded for that identifier
- **AND** the system SHALL send the upstream model identifier rather than the public alias

#### Scenario: An unknown model is requested
- **WHEN** a request names a model identifier that is not configured
- **THEN** the system SHALL reject the request without contacting an upstream provider
- **AND** the response SHALL identify the requested model as unsupported

### Requirement: Prevent client-controlled provider selection
The system SHALL derive the upstream provider exclusively from the resolved model mapping.

#### Scenario: A request attempts to override the provider
- **WHEN** a request supplies a provider selector that conflicts with or bypasses the model mapping
- **THEN** the system SHALL reject the request without contacting an upstream provider

### Requirement: Validate provider-specific options
The system SHALL accept namespaced provider options only when they belong to the provider selected by the model mapping and are supported by that provider adapter.

#### Scenario: Matching provider options are supplied
- **WHEN** a request supplies supported options for the resolved provider
- **THEN** the system SHALL apply those options to the provider-native upstream request

#### Scenario: Options target another provider
- **WHEN** a request supplies provider options for a provider other than the resolved provider
- **THEN** the system SHALL reject the request rather than silently ignore the options

#### Scenario: An option is unsupported
- **WHEN** a request supplies an unknown or unsupported option for the resolved provider
- **THEN** the system SHALL reject the request with an explanatory validation error

### Requirement: Preserve provider-native execution
The system SHALL translate the normalized request into the selected provider's native protocol and SHALL preserve structured output required to produce the client protocol response.

#### Scenario: A Codex model is selected
- **WHEN** the model mapping selects the Codex provider
- **THEN** the system SHALL call the Codex Responses upstream using Codex-specific credentials and request behavior

#### Scenario: An Anthropic model is selected
- **WHEN** the model mapping selects the Anthropic provider
- **THEN** the system SHALL call the Anthropic Messages upstream using Anthropic-specific credentials and request behavior
