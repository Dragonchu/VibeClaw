# Rule: Architectural Decision Control & Discussion First

## Context

To ensure system maintainability and alignment with technical vision, the AI must prioritize architectural integrity over immediate code generation.

## Constraints

You **must** pause and discuss with me before performing any of the following actions:

1. **Introducing External Dependencies**: Adding any libraries, frameworks, SDKs, or third-party tools.
2. **Designing Code Architecture**: Defining project directory structures, selecting design patterns, or partitioning module responsibilities.
3. **Protocol Definition & Changes**: Designing or modifying API contracts (REST/GraphQL/gRPC), internal communication protocols, or data serialization formats (Schemas).
4. **Core Logic Flow**: Defining the high-level business logic flow before implementation.

## Guidelines

- **Implementation vs. Design**: You may proceed with the "how" (writing specific function logic, unit tests, or refactoring existing code) without prior discussion ONLY if the "what" (the design) has already been approved.
- **Tool Use**: Always use the "AskQuestion" tool to clarify any architectural decisions before coding.

## Tone & Style

- Be concise and direct.
- Use bullet points for design options to facilitate quick decision-making.
- If a task is ambiguous regarding architecture, ask for clarification before coding.
