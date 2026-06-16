[![Docker Image](https://img.shields.io/github/actions/workflow/status/TABmk/TABNotes.md/docker-image.yml?label=docker%20publish)](https://github.com/TABmk/TABNotes.md/actions/workflows/docker-image.yml)
[![Docker Pulls](https://img.shields.io/docker/pulls/tabmk/tabnotes?logo=docker)](https://hub.docker.com/r/tabmk/tabnotes)
[![Issues](https://img.shields.io/github/issues/TABmk/TABNotes.md)](https://github.com/TABmk/TABNotes.md/issues)
[![Pull Requests](https://img.shields.io/github/issues-pr/TABmk/TABNotes.md)](https://github.com/TABmk/TABNotes.md/pulls)
[![License](https://img.shields.io/github/license/TABmk/TABNotes.md)](https://github.com/TABmk/TABNotes.md/blob/master/LICENSE)
[![Stars](https://img.shields.io/github/stars/TABmk/TABNotes.md?style=social)](https://github.com/TABmk/TABNotes.md)

# TABNotes.md

<p align="center">
  <img src="preview/preview.png" />
</p>

TABNotes is a lightweight self-hosted Markdown notes service written in Rust. It keeps the model simple: one admin account, SQLite storage, clean sharing modes, and an API for scripted note publishing.

## Quick Install

Run the published image with the smallest practical setup:

```bash
docker run -d \
  --name tabnotes \
  --restart unless-stopped \
  -p 8080:8080 \
  -e ADMIN_USERNAME=admin \
  -e ADMIN_PASSWORD='change-this-now' \
  -e PUBLIC_BASE_URL='http://localhost:8080' \
  -v tabnotes_data:/app/data \
  tabmk/tabnotes
```

Open `http://localhost:8080/login` and sign in with the configured admin credentials. For public deployment, set `PUBLIC_BASE_URL` to your real HTTPS URL.

## Features

- Single admin account with no self-registration flow
- Markdown editor with live preview
- Three visibility modes: `admin`, `public`, and `code`
- Shared rendered note pages with clean URLs
- API key management for scripted publishing and automation
- Passkey support for admin login
- SQLite storage with a minimal dashboard
- Ready for Docker, Docker Compose, or local Rust runs

## Screenshots

<p align="center" width="100%">
  <img width="49%" src="preview/1.png" />
  <img width="49%" src="preview/2.png" />
</p>
<p align="center" width="100%">
  <img width="49%" src="preview/3.png" />
  <img width="49%" src="preview/4.png" />
</p>

<details>
<summary><b><u><font size="+1">Agent Skill</font></u></b></summary>

The project includes an agent skill for publishing TABNotes through the API with bundled `curl` scripts and built-in environment validation. An agent can install it by pointing at [https://github.com/TABmk/TABNotes.md/tree/master/skill](https://github.com/TABmk/TABNotes.md/tree/master/skill).
</details>

<details>
<summary><b><u><font size="+1">Docker Compose</font></u></b></summary>

Example `compose.yaml`:

```yaml
services:
  tabnotes:
    image: tabmk/tabnotes:latest
    container_name: tabnotes
    restart: unless-stopped
    ports:
      - "8080:8080"
    environment:
      ADMIN_USERNAME: admin
      ADMIN_PASSWORD: change-this-now
      PUBLIC_BASE_URL: http://localhost:8080
      ROOT_REDIRECT_URL: /dashboard
      NOTES_PATH_PREFIX: notes
      DATABASE_URL: sqlite://data/tabnotes.db
      PASSKEY_RP_NAME: TabNotes
      HIDE_FOOTER: "false"
      HIDE_SWAGGER: "true"
      HIDE_API_DOCS: "false"
      RUST_LOG: info
    volumes:
      - tabnotes_data:/app/data

volumes:
  tabnotes_data:
```

Run it with:

```bash
docker compose up -d
```

If you change environment variables later, recreate the container:

```bash
docker compose up -d --force-recreate
```
</details>

<details>
<summary><b><u><font size="+1">Docker Run</font></u></b></summary>

Run the published image directly:

```bash
docker run -d \
  --name tabnotes \
  --restart unless-stopped \
  -p 8080:8080 \
  -e ADMIN_USERNAME=admin \
  -e ADMIN_PASSWORD='change-this-now' \
  -e PUBLIC_BASE_URL='http://localhost:8080' \
  -e ROOT_REDIRECT_URL='/dashboard' \
  -e NOTES_PATH_PREFIX='notes' \
  -e DATABASE_URL='sqlite://data/tabnotes.db' \
  -e PASSKEY_RP_NAME='TabNotes' \
  -e HIDE_FOOTER='false' \
  -e HIDE_SWAGGER='true' \
  -e HIDE_API_DOCS='false' \
  -e RUST_LOG='info' \
  -v tabnotes_data:/app/data \
  tabmk/tabnotes
```

If you prefer a host bind mount instead of a named volume:

```bash
mkdir -p data
sudo chown -R 10001:10001 data
```

Then replace the volume flag with:

```bash
-v "$(pwd)/data:/app/data"
```
</details>

<details>
<summary><b><u><font size="+1">Environment Variables</font></u></b></summary>

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `ADMIN_USERNAME` | yes | - | Admin login username |
| `ADMIN_PASSWORD` | yes | - | Admin login password |
| `PUBLIC_BASE_URL` | yes | - | Public base URL used for shared links and passkeys |
| `BIND_ADDR` | no | `0.0.0.0:8080` | Listen address |
| `DATABASE_URL` | no | `sqlite://data/tabnotes.db` | SQLite connection string |
| `ROOT_REDIRECT_URL` | no | `/dashboard` | Redirect target for `/` |
| `NOTES_PATH_PREFIX` | no | `notes` | Shared note path prefix |
| `PASSKEY_RP_NAME` | no | `TabNotes` | WebAuthn relying-party name |
| `HIDE_FOOTER` | no | `false` | Hide the footer |
| `HIDE_SWAGGER` | no | `true` | Expose Swagger UI when set to `false` |
| `HIDE_API_DOCS` | no | `false` | Hide `/api-docs/openapi.json` when set to `true` |
| `RUST_LOG` | no | `info` | Rust log filter |
</details>

<details>
<summary><b><u><font size="+1">Local Development</font></u></b></summary>

```bash
export ADMIN_USERNAME=admin
export ADMIN_PASSWORD='change-this'
export PUBLIC_BASE_URL='http://localhost:8080'
export ROOT_REDIRECT_URL='/dashboard'
export NOTES_PATH_PREFIX='notes'

cargo run
```

Then open `http://localhost:8080/login`.
</details>

<details>
<summary><b><u><font size="+1">Passkeys</font></u></b></summary>

Passkeys require a secure origin:

- `https://your-domain.example`
- `http://localhost` for local development

If the app runs behind a proxy or on an internal network, the browser still needs to access it through a valid secure origin.
</details>

<details>
<summary><b><u><font size="+1">API Usage</font></u></b></summary>

Create an API key from the dashboard. The raw key is shown only once, so store it immediately.

Send the key in either header:

- `X-API-Key: tn_...`
- `Authorization: Bearer tn_...`

```bash
API_KEY='tn_your_key_here'

curl -H "X-API-Key: $API_KEY" http://localhost:8080/api/notes

curl -X POST http://localhost:8080/api/notes \
  -H "Authorization: Bearer $API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{
    "title": "API note",
    "slug": "api-note",
    "markdown": "# API note\n\nCreated over HTTP.",
    "visibility": "public"
  }'
```
</details>
