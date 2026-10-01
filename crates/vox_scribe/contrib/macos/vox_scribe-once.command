#!/bin/zsh
# One-shot dictation in a Terminal window. Launch it with
# `open -a Terminal vox_scribe-once.command` (see karabiner-vox_scribe.json)
# so macOS grants the microphone to Terminal, not to the launcher.
"${0:A:h}/vox_scribe" --once
exit
