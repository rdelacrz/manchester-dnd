#!/usr/bin/env bash

set -Eeuo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

declare -a files=()
while IFS= read -r -d '' file; do
    [[ -f "$file" ]] && files+=("$file")
done < <(git ls-files --cached --others --exclude-standard -z)

for scan_path in "$@"; do
    if [[ -f "$scan_path" ]]; then
        files+=("$scan_path")
    elif [[ -d "$scan_path" ]]; then
        while IFS= read -r -d '' file; do
            files+=("$file")
        done < <(find "$scan_path" -type f -print0)
    else
        echo "secret scan: path does not exist: $scan_path" >&2
        exit 1
    fi
done

if [[ ${#files[@]} -eq 0 ]]; then
    echo "secret scan: no files to inspect" >&2
    exit 1
fi

declare -a canaries=()
if [[ -n "${SECRET_SCAN_CANARIES:-}" ]]; then
    while IFS= read -r canary; do
        [[ -n "$canary" ]] && canaries+=("$canary")
    done <<< "$SECRET_SCAN_CANARIES"
fi

if [[ "${SECRET_SCAN_REQUIRE_CANARIES:-0}" == "1" && ${#canaries[@]} -eq 0 ]]; then
    echo "secret scan: SECRET_SCAN_CANARIES is required for this run" >&2
    exit 1
fi

for canary in "${canaries[@]}"; do
    if (( ${#canary} < 20 )); then
        echo "secret scan: every canary must contain at least 20 characters" >&2
        exit 1
    fi
done

declare -a leaks=()
for canary in "${canaries[@]}"; do
    for file in "${files[@]}"; do
        if LC_ALL=C grep --binary-files=text --fixed-strings --quiet -- "$canary" "$file"; then
            leaks+=("$file")
        fi
    done
done

if [[ ${#leaks[@]} -ne 0 ]]; then
    echo "secret scan: injected canary material was found in:" >&2
    printf '  %s\n' "${leaks[@]}" | LC_ALL=C sort -u >&2
    exit 1
fi

# These deliberately narrow signatures catch common high-confidence credentials
# without treating local example passwords or ordinary identifiers as secrets.
credential_pattern='-----BEGIN (RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9_]{30,}|github_pat_[A-Za-z0-9_]{20,}|sk-(proj-)?[A-Za-z0-9_-]{24,}'
declare -a signature_hits=()
for file in "${files[@]}"; do
    if LC_ALL=C grep --binary-files=without-match --extended-regexp --quiet -- "$credential_pattern" "$file"; then
        signature_hits+=("$file")
    fi
done

if [[ ${#signature_hits[@]} -ne 0 ]]; then
    echo "secret scan: high-confidence credential signature found in:" >&2
    printf '  %s\n' "${signature_hits[@]}" | LC_ALL=C sort -u >&2
    exit 1
fi

if [[ ${#canaries[@]} -eq 0 ]]; then
    echo "secret scan: credential signatures passed; injected-canary check was not requested"
else
    echo "secret scan: ${#canaries[@]} injected canaries and credential signatures were absent from ${#files[@]} files"
fi
