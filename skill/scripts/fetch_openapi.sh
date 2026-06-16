#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: fetch_openapi.sh [--pretty]

Options:
  --pretty     Pretty-print JSON output
  -h, --help   Show this help
EOF
}

fail() {
  printf '%s\n' "$1" >&2
  exit "${2:-1}"
}

require_env() {
  local name="$1"
  local value="${!name:-}"
  if [[ -z "${value// }" ]]; then
    fail "Missing required environment variable: $name"
  fi
}

pretty="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pretty)
      pretty="true"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "Unknown argument: $1"
      ;;
  esac
done

require_env TABNOTES_URL

response_tmp="$(mktemp)"
curl_err_tmp="$(mktemp)"
trap 'rm -f "$response_tmp" "$curl_err_tmp"' EXIT

base_url="${TABNOTES_URL%/}"
if ! http_code="$(
  curl -sS \
    -o "$response_tmp" \
    -w '%{http_code}' \
    -H 'Accept: application/json' \
    "${base_url}/api-docs/openapi.json" \
    2>"$curl_err_tmp"
)"; then
  curl_error="$(tr '\n' ' ' <"$curl_err_tmp" | sed 's/[[:space:]]\+/ /g; s/^ //; s/ $//')"
  fail "Failed to reach TABNote OpenAPI endpoint with curl${curl_error:+: $curl_error}"
fi

if [[ "$http_code" -lt 200 || "$http_code" -ge 300 ]]; then
  fail "TABNote OpenAPI endpoint returned HTTP ${http_code}: $(cat "$response_tmp")"
fi

if [[ "$pretty" == "true" ]]; then
  LC_ALL=C LANG=C perl -MJSON::PP -e '
    use strict;
    use warnings;
    local $/;
    my $body = <STDIN>;
    my $decoded = decode_json($body);
    print JSON::PP->new->ascii->pretty->encode($decoded);
  ' <"$response_tmp"
else
  cat "$response_tmp"
fi
