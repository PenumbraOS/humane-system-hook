#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ALIUHOOK_VERSION="1.1.4"
ALIUHOOK_BASE_URL="https://github.com/agg23/Aliuhook/releases/download/${ALIUHOOK_VERSION}"
ALIUHOOK_DIR="${ROOT_DIR}/.ci/m2/com/aliucord/Aliuhook/${ALIUHOOK_VERSION}"

mkdir -p "${ALIUHOOK_DIR}"

curl -L --fail --retry 5 --retry-delay 5 \
  -o "${ALIUHOOK_DIR}/Aliuhook-${ALIUHOOK_VERSION}.aar" \
  "${ALIUHOOK_BASE_URL}/Aliuhook-${ALIUHOOK_VERSION}.aar"
curl -L --fail --retry 5 --retry-delay 5 \
  -o "${ALIUHOOK_DIR}/Aliuhook-${ALIUHOOK_VERSION}.module" \
  "${ALIUHOOK_BASE_URL}/Aliuhook-${ALIUHOOK_VERSION}.module"
curl -L --fail --retry 5 --retry-delay 5 \
  -o "${ALIUHOOK_DIR}/Aliuhook-${ALIUHOOK_VERSION}.pom" \
  "${ALIUHOOK_BASE_URL}/Aliuhook-${ALIUHOOK_VERSION}.pom"

echo "d3ca4a36866a7fd6709e25463484bbd10b47fb298b36bb499d91a8cac662c714  ${ALIUHOOK_DIR}/Aliuhook-${ALIUHOOK_VERSION}.aar" | sha256sum -c -
echo "10d52677d086c3d628c1fe14220d618a6ff9f36bdc7a806c4f105d948f33d7ee  ${ALIUHOOK_DIR}/Aliuhook-${ALIUHOOK_VERSION}.module" | sha256sum -c -
echo "957d2f3fc68a7e4d0e752056c5a9a99892a75c2302a91312608ef0b21b8d5b90  ${ALIUHOOK_DIR}/Aliuhook-${ALIUHOOK_VERSION}.pom" | sha256sum -c -

printf 'Bootstrapped Aliuhook %s into %s\n' "${ALIUHOOK_VERSION}" "${ALIUHOOK_DIR}"
