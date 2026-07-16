#!/usr/bin/env bash
set -euo pipefail

OUTPUT_DIR=${1:-.runtime-private/supply-chain-tools/bin}
SYFT_VERSION=1.46.0
GRYPE_VERSION=0.115.0

case "$(uname -m)" in
  x86_64)
    archive_arch=amd64
    syft_sha256=d654f678b709eb53c393d38519d5ed7d2e57205529404018614cfefa0fb2b5ca
    grype_sha256=3fad92940650e514c0aa2dad83526942a055e210cec09a8a59d9c024adc2b90e
    ;;
  aarch64|arm64)
    archive_arch=arm64
    syft_sha256=9fafef4db4f032ce81008d3a1529985d41ceb6ccdf2b388c9ce2f1ed7d32082e
    grype_sha256=b8541b9ecc3e936e7db4ff14b71a9474b25f3898ccaad63ee0bfe3449fcd734d
    ;;
  *)
    echo "unsupported supply-chain tool architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
install -d -m 0755 "$OUTPUT_DIR"

download_and_install() {
  local name=$1
  local version=$2
  local expected_sha256=$3
  local archive="${name}_${version}_linux_${archive_arch}.tar.gz"
  local url="https://github.com/anchore/${name}/releases/download/v${version}/${archive}"

  curl --fail --silent --show-error --location --output "$work_dir/$archive" "$url"
  printf '%s  %s\n' "$expected_sha256" "$work_dir/$archive" | sha256sum --check --status
  tar --extract --gzip --file "$work_dir/$archive" --directory "$work_dir" "$name"
  install -m 0755 "$work_dir/$name" "$OUTPUT_DIR/$name"
}

download_and_install syft "$SYFT_VERSION" "$syft_sha256"
download_and_install grype "$GRYPE_VERSION" "$grype_sha256"

[[ "$($OUTPUT_DIR/syft version -o json | jq -r .version)" == "$SYFT_VERSION" ]]
[[ "$($OUTPUT_DIR/grype version -o json | jq -r .version)" == "$GRYPE_VERSION" ]]

echo "installed pinned supply-chain tools: syft ${SYFT_VERSION}, grype ${GRYPE_VERSION}"
