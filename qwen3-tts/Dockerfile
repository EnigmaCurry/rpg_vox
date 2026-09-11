# syntax=docker/dockerfile:1.7

# Use the upstream vLLM-Omni image so the CUDA / PyTorch / vLLM / vLLM-Omni
# versions are all pinned together by the maintainers. First attempt at a
# hand-rolled install landed on incompatible cu128 torchaudio vs cu130
# torch (vLLM 0.29 dragged torch up); tracking upstream's pinned combo is
# both simpler and less fragile.
#
# The base image intentionally has no ENTRYPOINT — we add ours below so
# `docker compose up` runs `vllm serve` with the right args.
FROM vllm/vllm-omni:v0.28.0

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    HF_HOME=/models \
    HUGGINGFACE_HUB_CACHE=/models \
    # HF_HUB_ENABLE_HF_TRANSFER is deprecated (hf_transfer is no longer
    # used); Xet is the current fast-transfer path.
    HF_XET_HIGH_PERFORMANCE=1 \
    # vLLM's sampler JIT-compiles through FlashInfer; on consumer/pro
    # Blackwell (sm_120) that build path can die inside profile_run and
    # take the engine with it. vllm-omni's TTS example README calls this
    # out — flip to the CUDA sampler.
    VLLM_USE_FLASHINFER_SAMPLER=0 \
    # POST /v1/audio/voices persists .safetensors here. Backed by the
    # /models volume so uploaded clones survive container recreation.
    SPEAKER_SAMPLES_DIR=/models/speakers \
    SPEAKER_MAX_UPLOADED=1000

RUN mkdir -p /models

EXPOSE 8000

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
