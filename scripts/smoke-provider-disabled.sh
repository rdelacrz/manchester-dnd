#!/usr/bin/env bash
set -euo pipefail

port=${SMOKE_PORT:-6789}
log_file=$(mktemp)
server_pid=''

cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -f "$log_file"
}
trap cleanup EXIT

APP_ACCESS_MODE=local \
LEPTOS_SITE_ADDR="127.0.0.1:${port}" \
TEXT_LLM_BACKEND=disabled \
IMAGE_LLM_BACKEND=disabled \
cargo leptos serve --release >"$log_file" 2>&1 &
server_pid=$!

for _ in $(seq 1 90); do
  if curl --fail --silent --output /dev/null "http://127.0.0.1:${port}/health/ready"; then
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$log_file" >&2
    exit 1
  fi
  sleep 1
done

curl --fail --silent --show-error "http://127.0.0.1:${port}/health/live" --output /dev/null
page=$(curl --fail --silent --show-error "http://127.0.0.1:${port}/")
if [[ "$page" != *"Manchester Arcana"* ]]; then
  printf 'provider-disabled smoke test did not render the application shell\n' >&2
  exit 1
fi

if rg --quiet 'api\.openai\.com|TEXT_LLM_API_KEY|IMAGE_LLM_API_KEY' "$log_file"; then
  printf 'provider-disabled smoke test observed provider activity or a secret name\n' >&2
  exit 1
fi

