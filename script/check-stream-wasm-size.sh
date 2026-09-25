#!/usr/bin/env sh
set -eu

budget_file="${1:-contracts/stream/wasm-size-budget.env}"

if [ ! -f "$budget_file" ]; then
  echo "missing wasm size budget file: $budget_file" >&2
  exit 1
fi

# shellcheck disable=SC1090
. "$budget_file"

if [ -n "${ARTIFACT_OVERRIDE:-}" ]; then
  ARTIFACT="$ARTIFACT_OVERRIDE"
fi
if [ -n "${BUILD_COMMAND_OVERRIDE:-}" ]; then
  BUILD_COMMAND="$BUILD_COMMAND_OVERRIDE"
fi
if [ -n "${BASELINE_BYTES_OVERRIDE:-}" ]; then
  BASELINE_BYTES="$BASELINE_BYTES_OVERRIDE"
fi
if [ -n "${MAX_BYTES_OVERRIDE:-}" ]; then
  MAX_BYTES="$MAX_BYTES_OVERRIDE"
fi

: "${PACKAGE:?PACKAGE is required}"
: "${TARGET:?TARGET is required}"
: "${ARTIFACT:?ARTIFACT is required}"
: "${BUILD_COMMAND:?BUILD_COMMAND is required}"
: "${BASELINE_BYTES:?BASELINE_BYTES is required}"
: "${MAX_BYTES:?MAX_BYTES is required}"

case "$BASELINE_BYTES" in
  ''|*[!0-9]*)
    echo "BASELINE_BYTES must be an unsigned integer, got: $BASELINE_BYTES" >&2
    exit 1
    ;;
esac

case "$MAX_BYTES" in
  ''|*[!0-9]*)
    echo "MAX_BYTES must be an unsigned integer, got: $MAX_BYTES" >&2
    exit 1
    ;;
esac

echo "fluxora-stream wasm size budget"
echo "package: $PACKAGE"
echo "target: $TARGET"
echo "artifact: $ARTIFACT"
echo "baseline_bytes: $BASELINE_BYTES"
echo "max_bytes: $MAX_BYTES"
echo "build_command: $BUILD_COMMAND"

if [ "$BUILD_COMMAND" != "true" ]; then
  sh -c "$BUILD_COMMAND"
fi

if [ ! -f "$ARTIFACT" ]; then
  echo "expected wasm artifact was not produced: $ARTIFACT" >&2
  exit 1
fi

size=$(wc -c < "$ARTIFACT" | tr -d ' ')
delta=$((size - BASELINE_BYTES))

section_breakdown=$(python3 - "$ARTIFACT" <<'PY'
import sys

path = sys.argv[1]
with open(path, 'rb') as fh:
    data = fh.read()

if len(data) < 8 or data[:4] != b'\x00asm':
    print('section_breakdown: code=0 data=0 custom=0 total=0')
    raise SystemExit(0)

code = 0
custom = 0
data_size = 0
offset = 8
while offset < len(data):
    if offset + 1 > len(data):
        break
    sec_id = data[offset]
    offset += 1
    value = 0
    shift = 0
    while True:
        if offset >= len(data):
            break
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        if (byte & 0x80) == 0:
            break
        shift += 7
    if offset + value > len(data):
        break
    payload = data[offset:offset + value]
    offset += value
    if sec_id == 0:
        custom += len(payload)
    elif sec_id == 10:
        code += len(payload)
    elif sec_id == 11:
        data_size += len(payload)

print(f'section breakdown: code={code} data={data_size} custom={custom} total={len(data)}')
PY
)

printf '%s\n' "$section_breakdown"
printf 'fluxora_stream.wasm: %s bytes\n' "$size"
printf 'baseline_delta_bytes: %s\n' "$delta"

if [ "$size" -gt "$MAX_BYTES" ]; then
  echo "WASM size ${size} exceeds budget ${MAX_BYTES} bytes" >&2
  echo "Update $budget_file only when growth is intentional and reviewed." >&2
  exit 1
fi

