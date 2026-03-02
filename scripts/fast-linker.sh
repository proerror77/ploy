#!/usr/bin/env bash
set -euo pipefail

if command -v clang >/dev/null 2>&1; then
  linker_driver="clang"
elif command -v cc >/dev/null 2>&1; then
  linker_driver="cc"
elif command -v gcc >/dev/null 2>&1; then
  linker_driver="gcc"
else
  echo "No C compiler available for linking" >&2
  exit 1
fi

if command -v mold >/dev/null 2>&1; then
  exec "$linker_driver" -fuse-ld=mold "$@"
fi

if command -v ld.lld >/dev/null 2>&1 || command -v lld >/dev/null 2>&1; then
  exec "$linker_driver" -fuse-ld=lld "$@"
fi

exec "$linker_driver" "$@"
