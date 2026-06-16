#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: publish_note.sh --title TITLE [options]

Options:
  --title TITLE           Note title (required)
  --slug SLUG             Optional slug
  --visibility VALUE      admin, public, or code (default: admin)
  --access-code CODE      Required when visibility is code
  --markdown TEXT         Markdown text
  --markdown-file PATH    Read markdown from a file
  -h, --help              Show this help

Provide markdown with exactly one of:
  --markdown
  --markdown-file
  stdin
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

title=""
slug=""
visibility="admin"
access_code=""
markdown_arg=""
markdown_file=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --title)
      [[ $# -ge 2 ]] || fail "--title requires a value"
      title="$2"
      shift 2
      ;;
    --slug)
      [[ $# -ge 2 ]] || fail "--slug requires a value"
      slug="$2"
      shift 2
      ;;
    --visibility)
      [[ $# -ge 2 ]] || fail "--visibility requires a value"
      visibility="$2"
      shift 2
      ;;
    --access-code)
      [[ $# -ge 2 ]] || fail "--access-code requires a value"
      access_code="$2"
      shift 2
      ;;
    --markdown)
      [[ $# -ge 2 ]] || fail "--markdown requires a value"
      markdown_arg="$2"
      shift 2
      ;;
    --markdown-file)
      [[ $# -ge 2 ]] || fail "--markdown-file requires a value"
      markdown_file="$2"
      shift 2
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

[[ -n "$title" ]] || fail "--title is required"

case "$visibility" in
  admin|public|code) ;;
  *)
    fail "--visibility must be one of: admin, public, code"
    ;;
esac

if [[ "$visibility" == "code" && -z "$access_code" ]]; then
  fail "--access-code is required when --visibility=code"
fi

if [[ "$visibility" != "code" && -n "$access_code" ]]; then
  fail "--access-code is only valid when --visibility=code"
fi

markdown_tmp="$(mktemp)"
response_tmp="$(mktemp)"
curl_err_tmp="$(mktemp)"
trap 'rm -f "$markdown_tmp" "$response_tmp" "$curl_err_tmp"' EXIT

if [[ -n "$markdown_arg" ]]; then
  [[ -z "$markdown_file" ]] || fail "Use exactly one markdown source: --markdown, --markdown-file, or stdin."
  printf '%s' "$markdown_arg" >"$markdown_tmp"
elif [[ -n "$markdown_file" ]]; then
  [[ -f "$markdown_file" ]] || fail "Markdown file not found: $markdown_file"
  cp "$markdown_file" "$markdown_tmp"
else
  [[ ! -t 0 ]] || fail "Provide markdown via --markdown, --markdown-file, or stdin."
  cat >"$markdown_tmp"
fi

if [[ ! -s "$markdown_tmp" ]]; then
  fail "Provide markdown via --markdown, --markdown-file, or stdin."
fi

require_env TABNOTES_URL
require_env TABNOTES_API_KEY

payload="$(
  TABNOTE_TITLE="$title" \
  TABNOTE_SLUG="$slug" \
  TABNOTE_VISIBILITY="$visibility" \
  TABNOTE_ACCESS_CODE="$access_code" \
  TABNOTE_MARKDOWN_FILE="$markdown_tmp" \
  LC_ALL=C LANG=C perl -MJSON::PP -e '
    use strict;
    use warnings;
    use Encode qw(decode);
    local $/;
    open my $fh, "<:raw", $ENV{TABNOTE_MARKDOWN_FILE} or die "Failed to read markdown\n";
    my $markdown = decode("UTF-8", <$fh>, 1);
    my %payload = (
      title => decode("UTF-8", $ENV{TABNOTE_TITLE}, 1),
      markdown => $markdown,
      visibility => $ENV{TABNOTE_VISIBILITY},
    );
    $payload{slug} = decode("UTF-8", $ENV{TABNOTE_SLUG}, 1) if length $ENV{TABNOTE_SLUG};
    $payload{access_code} = decode("UTF-8", $ENV{TABNOTE_ACCESS_CODE}, 1) if length $ENV{TABNOTE_ACCESS_CODE};
    print encode_json(\%payload);
  '
)"

base_url="${TABNOTES_URL%/}"
if ! http_code="$(
  curl -sS \
    -o "$response_tmp" \
    -w '%{http_code}' \
    -X POST "${base_url}/api/notes" \
    -H 'Content-Type: application/json' \
    -H "Authorization: Bearer ${TABNOTES_API_KEY}" \
    --data "$payload" \
    2>"$curl_err_tmp"
)"; then
  curl_error="$(tr '\n' ' ' <"$curl_err_tmp" | sed 's/[[:space:]]\+/ /g; s/^ //; s/ $//')"
  fail "Failed to reach TABNote API with curl${curl_error:+: $curl_error}"
fi

if [[ "$http_code" -lt 200 || "$http_code" -ge 300 ]]; then
  fail "TABNote API returned HTTP ${http_code}: $(cat "$response_tmp")"
fi

LC_ALL=C LANG=C perl -MJSON::PP -e '
  use strict;
  use warnings;
  binmode STDOUT, ":encoding(UTF-8)";
  local $/;
  my $body = <STDIN>;
  my $decoded = decode_json($body);
  print JSON::PP->new->pretty->encode($decoded);
' <"$response_tmp"
