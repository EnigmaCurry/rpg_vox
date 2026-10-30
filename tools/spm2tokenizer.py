#!/usr/bin/env python3
"""Write a tokenizer.json beside a Marian model's source.spm.

CTranslate2 is given token *strings* and maps them to ids itself from
the model's shared_vocabulary.json, so this only has to split text into
the same SentencePiece pieces the .spm would -- which the pure-Rust
`tokenizers` crate can do, with no protobuf and no C++ anywhere near
the room's binary.

  spm2tokenizer.py MODEL_DIR...
"""
import json
import sys
from pathlib import Path

import sentencepiece as spm
from tokenizers import Tokenizer, decoders, models, pre_tokenizers, processors

# What SentencePiece writes in place of a space.
SPACE = "▁"


def convert(model_dir: Path) -> None:
    sp = spm.SentencePieceProcessor(model_file=str(model_dir / "source.spm"))
    vocab = [(sp.IdToPiece(i), sp.GetScore(i)) for i in range(sp.GetPieceSize())]
    tok = Tokenizer(models.Unigram(vocab, unk_id=sp.unk_id(), byte_fallback=False))
    # Standard SentencePiece: a space becomes ▁, and one is added at the
    # front, which is what "▁Yes" rather than "Yes" depends on.
    tok.pre_tokenizer = pre_tokenizers.Metaspace(replacement=SPACE, prepend_scheme="always")
    tok.decoder = decoders.Metaspace(replacement=SPACE, prepend_scheme="always")
    # Marian has no bos; the eos is appended, which is what the
    # sentencepiece path in ct2rs does by hand.
    tok.post_processor = processors.TemplateProcessing(
        single="$A </s>", special_tokens=[("</s>", sp.eos_id())]
    )
    out = model_dir / "tokenizer.json"
    tok.save(str(out))

    # It is worth nothing at all if it doesn't split text the same way.
    checked = 0
    for line in [
        "Yes, extremely hot.",
        "A dragon is a legendary creature, often imagined as a giant reptile.",
        "Check https://example.com/a_b-c?q=1&r=2 before the meeting.",
        "Да, конечно.",
        "Родион Потапыч высчитывал каждый новый вершок углубления.",
        "100% — and that's that.",
        "",
    ]:
        want = sp.encode(line, out_type=str) + ["</s>"]
        got = tok.encode(line).tokens
        if want != got:
            raise SystemExit(
                f"{model_dir}: tokenizer.json disagrees with source.spm\n"
                f"  line: {line!r}\n  spm:  {want}\n  json: {got}"
            )
        checked += 1
    print(f"{out}  ({len(vocab)} pieces, {checked} lines agree with source.spm)")


for d in sys.argv[1:]:
    convert(Path(d))
