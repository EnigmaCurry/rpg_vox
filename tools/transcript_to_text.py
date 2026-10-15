#!/usr/bin/env python3

import argparse
import json
from pathlib import Path


def clean_transcript(data):
    lines = []

    current_channel = None
    current_text = []

    for paragraph in data.get("paragraphs", []):
        channel = paragraph.get("channel")
        text = paragraph.get("text", "").strip()

        if not channel or not text:
            continue

        if channel == current_channel:
            current_text.append(text)
        else:
            if current_channel is not None:
                lines.append(
                    f"{current_channel}: {' '.join(current_text)}"
                )

            current_channel = channel
            current_text = [text]

    if current_channel is not None:
        lines.append(
            f"{current_channel}: {' '.join(current_text)}"
        )

    return "\n\n".join(lines)


def main():
    parser = argparse.ArgumentParser(
        description="Convert transcription JSON to compact dialogue text."
    )
    parser.add_argument("input", type=Path, help="Input JSON file")
    parser.add_argument(
        "output",
        type=Path,
        nargs="?",
        help="Output text file (defaults to input filename with .txt)",
    )
    args = parser.parse_args()

    output = args.output or args.input.with_suffix(".txt")

    with args.input.open("r", encoding="utf-8") as f:
        data = json.load(f)

    transcript = clean_transcript(data)

    output.write_text(transcript + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
