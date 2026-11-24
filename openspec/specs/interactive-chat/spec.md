# Interactive Chat Specification

## Purpose

Define the terminal chat mode that maintains an in-memory Responses API conversation and renders streamed text.

## Requirements

### Requirement: Run an interactive terminal conversation

The system SHALL provide an interactive chat mode selected by the `chat` subcommand.

#### Scenario: Submit a prompt

- **WHEN** the user enters a non-empty line
- **THEN** the system SHALL add it to the in-memory conversation as a user message
- **AND** it SHALL request and display an assistant response
- **AND** it SHALL add a successful assistant response to the same conversation

#### Scenario: Ignore empty input

- **WHEN** the user enters an empty or whitespace-only line
- **THEN** the system SHALL prompt again without sending a request or changing conversation history

#### Scenario: Exit the chat

- **WHEN** the user enters `/exit` or `/quit`, or standard input reaches end-of-file
- **THEN** the system SHALL exit successfully

#### Scenario: A chat request fails

- **WHEN** an upstream request or stream fails
- **THEN** the system SHALL display the error
- **AND** it SHALL remove the failed user message so the unsuccessful turn is not retained in conversation history

### Requirement: Build streaming Responses API requests

The chat client SHALL send the full in-memory conversation as a non-persisted streaming Responses API request.

#### Scenario: Use the default model

- **WHEN** `OPENAI_MODEL` is not set
- **THEN** the request SHALL use model `gpt-5.5`
- **AND** it SHALL set `store` to `false`
- **AND** it SHALL set `stream` to `true`

#### Scenario: Override the model

- **WHEN** `OPENAI_MODEL` is set
- **THEN** the request SHALL use its value as the model identifier

### Requirement: Interpret streamed response events

The chat client SHALL extract assistant text from Responses API server-sent events.

#### Scenario: Text or refusal deltas are streamed

- **WHEN** the stream contains `response.output_text.delta` or `response.refusal.delta` events
- **THEN** the system SHALL concatenate their string deltas in arrival order
- **AND** it SHALL display the trimmed concatenated result

#### Scenario: Only a completed response is available

- **WHEN** the stream contains no text deltas and includes a `response.completed` event
- **THEN** the system SHALL extract text or refusal content from the completed response

#### Scenario: The stream reports failure

- **WHEN** the stream contains an `error` or `response.failed` event
- **THEN** the chat request SHALL fail with the upstream error message or detail when available

#### Scenario: The response contains no text

- **WHEN** a successful stream yields no text or refusal content
- **THEN** the system SHALL display `[no text output]`
- **AND** it SHALL retain that value as the assistant message in conversation history

### Requirement: Keep chat history ephemeral

The system SHALL keep interactive chat history only in process memory.

#### Scenario: The chat process exits

- **WHEN** interactive chat terminates
- **THEN** the system SHALL NOT persist its conversation history
