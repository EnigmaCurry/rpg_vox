#!/usr/bin/env bash
set -euo pipefail

MODEL="${QWEN_TTS_MODEL:-Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice}"
PORT="${QWEN_TTS_PORT:-8000}"
CONCURRENCY="${QWEN_TTS_CONCURRENCY:-4}"
DTYPE="${QWEN_TTS_DTYPE:-bfloat16}"

FLASH_FLAG="--flash-attn"
if [ "${QWEN_TTS_FLASH_ATTN:-1}" = "0" ]; then
  FLASH_FLAG="--no-flash-attn"
fi

echo "==> Qwen3-TTS starting"
echo "    model:       $MODEL"
echo "    port:        $PORT"
echo "    dtype:       $DTYPE"
echo "    flash-attn:  ${QWEN_TTS_FLASH_ATTN:-1}"
echo "    cache dir:   $HF_HOME"

# First run: pre-download so the demo starts responsive rather than
# blocking on a large download after Gradio has bound the port.
huggingface-cli download "$MODEL" >/dev/null

exec qwen-tts-demo "$MODEL" \
  --ip 0.0.0.0 \
  --port "$PORT" \
  --dtype "$DTYPE" \
  --concurrency "$CONCURRENCY" \
  $FLASH_FLAG \
  "$@"
