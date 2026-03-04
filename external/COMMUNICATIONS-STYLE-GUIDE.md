# External Communications Style Guide

Use this format for all future `REQUEST-*` and `RESPONSE-*` documents exchanged with downstream teams.

## Naming

- Request files: `REQUEST-<SYSTEM>-<TOPIC>-<TEAM>-<NNNN>.md`
- Response files: `RESPONSE-<SYSTEM>-<TOPIC>-<TEAM>-<NNNN>.md`
- Keep the response filename aligned to the request topic and sequence number.

## Required Metadata Block

Every response starts with:

```md
# RESPONSE-...

Request ID: `...`
Audience: ...
Requester: ...
Status: ...
Subject: ...
```

## Required Sections

Responses must include these sections in this order:

1. `## Summary`
2. `## Contract Clarification`
3. `## Implemented Surface`
4. `## C Header Definitions`
5. `## Status and Error Codes`
6. `## Ownership and Threading`
7. `## Integration Flow`
8. `## Build and Platform Notes`
9. `## Verification and Remaining Notes`

## Content Rules

- Use exact exported symbol names from the codebase.
- Include C definitions as fenced `c` blocks when new structs, enums, or function signatures are part of the contract.
- Call out any legacy or deprecated compatibility path explicitly.
- Separate implemented behavior from recommended downstream usage.
- State memory ownership and free functions with no ambiguity.
- State thread-safety and destroy-race rules with no ambiguity.
- Distinguish verified work from future packaging or release work.

## Status Terms

- `Implemented`: code landed and verified locally.
- `Implemented Pending Packaging`: code landed, but target artifacts were not built in this turn.
- `Clarified Only`: no code change, documentation/contract answer only.
- `Blocked`: request could not be completed; blockers must be listed in `Verification and Remaining Notes`.
