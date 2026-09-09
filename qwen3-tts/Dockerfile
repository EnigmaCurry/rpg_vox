# syntax=docker/dockerfile:1.7

# Blackwell (RTX PRO 6000, sm_120) requires CUDA 12.8+ and PyTorch 2.7+.
# Use the CUDA -devel image so we can build flash-attn from source.
FROM nvidia/cuda:12.8.1-cudnn-devel-ubuntu22.04

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PIP_NO_CACHE_DIR=1 \
    HF_HOME=/models \
    HUGGINGFACE_HUB_CACHE=/models \
    HF_HUB_ENABLE_HF_TRANSFER=1 \
    TORCH_CUDA_ARCH_LIST="12.0" \
    MAX_JOBS=4

# System deps. Python 3.12 comes from deadsnakes because 22.04 ships 3.10.
RUN apt-get update && apt-get install -y --no-install-recommends \
      software-properties-common ca-certificates gnupg curl git \
    && add-apt-repository -y ppa:deadsnakes/ppa \
    && apt-get update && apt-get install -y --no-install-recommends \
      python3.12 python3.12-dev python3.12-venv \
      build-essential ninja-build pkg-config \
      ffmpeg libsndfile1 sox libsox-dev \
    && curl -fsSL https://bootstrap.pypa.io/get-pip.py | python3.12 \
    && ln -sf /usr/bin/python3.12 /usr/local/bin/python \
    && ln -sf /usr/bin/python3.12 /usr/local/bin/python3 \
    && apt-get purge -y software-properties-common gnupg \
    && apt-get autoremove -y && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --upgrade pip setuptools wheel packaging

# PyTorch with CUDA 12.8 (Blackwell-compatible wheels).
RUN pip install --index-url https://download.pytorch.org/whl/cu128 \
      torch torchvision torchaudio

# HF CLI + fast transfer.
RUN pip install "huggingface_hub[cli]" hf_transfer

# Official Qwen3-TTS package (pins transformers==4.57.3, accelerate==1.12.0, gradio, etc.).
RUN pip install qwen-tts

# Flash-Attention 2 — compiled from source for Blackwell.
# This step is intentionally last so earlier layers cache well.
# Takes ~20-40 minutes on first build.
ARG BUILD_FLASH_ATTN=1
RUN if [ "$BUILD_FLASH_ATTN" = "1" ]; then \
      pip install -U flash-attn --no-build-isolation ; \
    else \
      echo "Skipping flash-attn build (BUILD_FLASH_ATTN=$BUILD_FLASH_ATTN)"; \
    fi

RUN mkdir -p /models
VOLUME ["/models"]

EXPOSE 8000

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
