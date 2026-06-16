---
name: publish-tabnote
description: Publish Markdown notes to a TABNote instance over its HTTP API. Use when Codex needs to create a new TABNote from markdown content, a markdown file, or generated text, especially when the user wants the note stored in their TABNote app instead of a local file. Prefer the bundled bash scripts, which use curl and validate required environment variables internally. Default new notes to admin visibility unless the user explicitly asks for public or code-protected access.
---

# Publish TABNote

## Overview

Use the bundled scripts to publish markdown into TABNote.
Treat `TABNOTES_URL` as the base app URL and `TABNOTES_API_KEY` as a secret.
Do not print or echo the API key.
Run TABNote network operations outside the sandbox.
Request escalated execution before invoking the bundled scripts instead of first attempting a sandboxed network call.

## Workflow

1. Confirm content source.
If the user gives markdown directly, pass it as markdown text.
If the user points to a local markdown file, pass the file path.

2. Choose visibility.
Default to `admin`.
Use `public` only when the user explicitly asks for a public note.
Use `code` only when the user explicitly asks for code-protected access. Require an access code in that case.

3. Prefer scripts over ad hoc shell.
Use `scripts/publish_note.sh` to create notes.
Use `scripts/fetch_openapi.sh` when the live API contract needs verification or the server behavior appears to have changed.
Let the scripts validate required environment variables instead of duplicating those checks in the skill call flow.
Request `require_escalated` for these script executions up front because TABNote access depends on external network connectivity.

## Scripts

### Publish note

Use `scripts/publish_note.sh`.
Run it with escalated execution immediately; do not burn a first attempt in the sandbox.

Examples:

```bash
bash scripts/publish_note.sh \
  --title "Release notes" \
  --markdown-file /path/to/release-notes.md
```

```bash
bash scripts/publish_note.sh \
  --title "Public changelog" \
  --visibility public \
  --markdown "# Changelog"
```

```bash
bash scripts/publish_note.sh \
  --title "Client handoff" \
  --visibility code \
  --access-code "shared-secret" \
  --markdown-file /path/to/handoff.md
```

Behavior:
- Defaults `--visibility` to `admin`
- Accepts markdown from `--markdown`, `--markdown-file`, or stdin
- Validates `TABNOTES_URL` and `TABNOTES_API_KEY` internally
- Should be invoked with escalated execution because it performs an external HTTP request
- Prints the created note JSON to stdout

### Fetch live OpenAPI

Use `scripts/fetch_openapi.sh`.
Run it with escalated execution immediately; do not first probe the endpoint from the sandbox.

Example:

```bash
bash scripts/fetch_openapi.sh --pretty
```

The live docs endpoint is always:

```text
TABNOTES_URL/api-docs/openapi.json
```

Use this script when:
- the API may have changed
- you need exact current request or response fields
- you want to inspect the deployed server contract before writing automation

## References

- Read `references/api.md` for environment rules, visibility defaults, and when to fetch live docs.
