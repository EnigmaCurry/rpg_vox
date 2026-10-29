#!/bin/sh
# Fetch the speech-to-text models vox_room needs, if the models volume
# doesn't have them yet, then hand over to the room.
#
# The layout and the archive names match crates/vox_scribe/src/models.rs,
# so a bind-mounted host models directory is found as-is and nothing is
# downloaded.
set -eu

MODELS="${VOX_SCRIBE_MODELS:-/models}"
STT="${VOX_ROOM_STT_MODEL:-parakeet}"
RELEASES="https://github.com/k2-fsa/sherpa-onnx/releases/download"

# The CLI subcommands only touch the database.
case "${1:-}" in
    invite | uninvite | people | help | --help | -h | --version | -V)
        exec vox_room "$@"
        ;;
esac

have() {
    for f in "$@"; do
        [ -e "$f" ] || return 1
    done
}

# fetch <release> <stem> <dest> <name-in-archive>:<name-on-disk>...
fetch() {
    release=$1
    stem=$2
    dest=$3
    shift 3
    tmp="$MODELS/.download/$stem"
    # Not /tmp: the Parakeet archive unpacks to most of a gigabyte, and
    # /tmp is a small tmpfs.
    rm -rf "$tmp"
    mkdir -p "$tmp"
    echo "vox_room: downloading $stem"
    curl -fL --retry 3 --progress-bar -o "$tmp/archive.tar.bz2" \
        "$RELEASES/$release/$stem.tar.bz2"
    tar -xjf "$tmp/archive.tar.bz2" -C "$tmp"
    mkdir -p "$dest"
    for pair in "$@"; do
        cp -r "$tmp/$stem/${pair%%:*}" "$dest/${pair##*:}"
    done
    rm -rf "$tmp"
    echo "vox_room: installed $dest"
}

case "$STT" in
    parakeet)
        d="$MODELS/parakeet-tdt-0.6b-v2"
        have "$d/encoder.int8.onnx" "$d/decoder.int8.onnx" \
            "$d/joiner.int8.onnx" "$d/tokens.txt" ||
            fetch asr-models sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8 "$d" \
                encoder.int8.onnx:encoder.int8.onnx \
                decoder.int8.onnx:decoder.int8.onnx \
                joiner.int8.onnx:joiner.int8.onnx \
                tokens.txt:tokens.txt
        ;;
    sensevoice)
        d="$MODELS/sense-voice"
        have "$d/model.int8.onnx" "$d/tokens.txt" ||
            fetch asr-models \
                sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17 "$d" \
                model.int8.onnx:model.int8.onnx tokens.txt:tokens.txt
        ;;
    none) ;;
    *)
        echo "vox_room: VOX_ROOM_STT_MODEL must be parakeet, sensevoice or none (got '$STT')" >&2
        exit 1
        ;;
esac

# The streaming model: the text that appears while someone is still talking.
if [ "$STT" != none ]; then
    d="$MODELS/streaming-zipformer"
    have "$d/encoder.onnx" "$d/decoder.onnx" "$d/joiner.onnx" "$d/tokens.txt" ||
        fetch asr-models sherpa-onnx-streaming-zipformer-en-20M-2023-02-17 "$d" \
            encoder-epoch-99-avg-1.int8.onnx:encoder.onnx \
            decoder-epoch-99-avg-1.onnx:decoder.onnx \
            joiner-epoch-99-avg-1.int8.onnx:joiner.onnx \
            tokens.txt:tokens.txt
fi

# The local voice, when there is no Qwen3-TTS server to talk to.
if [ "${VOX_ROOM_TTS:-}" = kokoro ]; then
    d="$MODELS/kokoro"
    have "$d/model.onnx" "$d/voices.bin" "$d/tokens.txt" \
        "$d/lexicon-us-en.txt" "$d/espeak-ng-data" ||
        fetch tts-models kokoro-multi-lang-v1_0 "$d" \
            model.onnx:model.onnx voices.bin:voices.bin \
            tokens.txt:tokens.txt lexicon-us-en.txt:lexicon-us-en.txt \
            espeak-ng-data:espeak-ng-data
fi

# --no-stt has no environment variable of its own.
if [ "$STT" = none ]; then
    set -- --no-stt "$@"
fi

exec vox_room "$@"
