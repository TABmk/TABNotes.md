# TABNote API Notes

## Environment

Require:
- `TABNOTES_URL`
- `TABNOTES_API_KEY`

Treat `TABNOTES_API_KEY` as a secret.
Do not print it, log it, or embed it into saved files.

Normalize the base URL by removing a trailing slash before building endpoints.
Let the bundled scripts perform the environment checks instead of repeating them in the skill workflow.
Run TABNote HTTP calls outside the sandbox.
When using Codex command tools, request escalated execution before calling the bundled scripts rather than first retrying after a sandbox DNS or network failure.

## Live API docs

The deployed OpenAPI document is always:

```text
${TABNOTES_URL%/}/api-docs/openapi.json
```

Prefer the bundled publishing script for ordinary note creation.
Fetch the live OpenAPI document only when the request shape may have changed or when the user asks for API details.

Use the curl-based scripts in `scripts/` as the default transport. Some deployed hosts may behave differently for non-curl clients at the edge, so keep the skill's automation aligned with curl unless the user explicitly asks for another client.
Treat both `scripts/publish_note.sh` and `scripts/fetch_openapi.sh` as networked operations that should be launched with escalated execution immediately.

## Visibility

Default created notes to:

```text
admin
```

Only use:
- `public` when the user explicitly asks for a public note
- `code` when the user explicitly asks for code protection

When visibility is `code`, require an access code.

## Preferred operation

For note creation, prefer:

```text
scripts/publish_note.sh
```

Avoid writing ad hoc curl commands unless the user explicitly asks for raw HTTP examples.
