#!/usr/bin/env bash
set -euo pipefail

MODEL="${QWEN_TTS_MODEL:-Qwen/Qwen3-TTS-12Hz-1.7B-CustomVoice}"
PORT="${QWEN_TTS_PORT:-8000}"

# vllm-omni bundles per-model deploy YAMLs inside the wheel. The Qwen3-TTS
# one enables async_chunk (streaming), CUDA graphs on Code2Wav, and both
# stages at max_num_seqs=64. Override any of that via --stage-overrides or
# a mounted YAML if we ever need workload-specific tuning.
#
# Path is hardcoded because the tag-pinned vllm/vllm-omni:v0.28.0 image
# always installs vllm-omni to this site-packages location. Earlier I
# tried resolving via `python -c 'import vllm_omni; ...'` but vllm-omni
# emits INFO log lines to stdout at import time, which contaminated the
# captured value. Re-verify this path if the base image tag is bumped.
DEPLOY_YAML=/usr/local/lib/python3.12/dist-packages/vllm_omni/deploy/qwen3_tts.yaml

echo "==> vLLM-Omni Qwen3-TTS starting"
echo "    model:       $MODEL"
echo "    port:        $PORT"
echo "    deploy yaml: $DEPLOY_YAML"
echo "    cache dir:   $HF_HOME"
echo "    speakers:    $SPEAKER_SAMPLES_DIR"

# Pre-download so the server binds the port promptly rather than after a
# fresh multi-GB download on first container start. `hf` is the current
# CLI; the older `huggingface-cli` shim in the base image errors out.
hf download "$MODEL" >/dev/null

# Ensure the speaker registry dir exists before vllm-omni's /v1/audio/voices
# handler tries to write into it.
mkdir -p "$SPEAKER_SAMPLES_DIR"

exec vllm serve "$MODEL" \
  --deploy-config "$DEPLOY_YAML" \
  --omni \
  --host 0.0.0.0 \
  --port "$PORT" \
  --stage-overrides '{"1":{"enforce_eager":true}}' \
  "$@"
