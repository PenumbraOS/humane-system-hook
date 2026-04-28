#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_VERSION="1.1.4"
RELEASE_BASE_URL="https://github.com/agg23/Aliuhook/releases/download/${RELEASE_VERSION}"
MAVEN_ROOT="${ROOT_DIR}/.ci/m2"

bootstrap_artifact() {
  local group_path="$1"
  local artifact_id="$2"
  local version="$3"
  local extension="$4"
  local sha256="$5"
  local artifact_dir="${MAVEN_ROOT}/${group_path}/${artifact_id}/${version}"
  local filename="${artifact_id}-${version}.${extension}"

  mkdir -p "${artifact_dir}"
  curl -L --fail --retry 5 --retry-delay 5 \
    -o "${artifact_dir}/${filename}" \
    "${RELEASE_BASE_URL}/${filename}"
  echo "${sha256}  ${artifact_dir}/${filename}" | sha256sum -c -
}

bootstrap_artifact "com/aliucord" "Aliuhook" "1.1.4" "aar" "d3ca4a36866a7fd6709e25463484bbd10b47fb298b36bb499d91a8cac662c714"
bootstrap_artifact "com/aliucord" "Aliuhook" "1.1.4" "module" "10d52677d086c3d628c1fe14220d618a6ff9f36bdc7a806c4f105d948f33d7ee"
bootstrap_artifact "com/aliucord" "Aliuhook" "1.1.4" "pom" "957d2f3fc68a7e4d0e752056c5a9a99892a75c2302a91312608ef0b21b8d5b90"

bootstrap_artifact "com/aliucord/lsplant" "lsplant" "6.4-aliucord.4" "aar" "9d6ecdd1831aac009d9325937efee94ca47363854d347d6d4142725ec97ecae2"
bootstrap_artifact "com/aliucord/lsplant" "lsplant" "6.4-aliucord.4" "module" "319fc14f7edc5c14b4373e2c164ddab53fd536eafebb11dab8c440ac87b06a95"
bootstrap_artifact "com/aliucord/lsplant" "lsplant" "6.4-aliucord.4" "pom" "3403658dd333ad57a345c1c22f4f3a5a513ed35524cf9ca044bc954083032400"

printf 'Bootstrapped mirrored Aliuhook dependencies into %s\n' "${MAVEN_ROOT}"
