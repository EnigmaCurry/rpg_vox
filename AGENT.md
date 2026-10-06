# agent

`agent` answers a [`scribe --chat`](SCRIBE.md#chat-with-a-responder)
conversation with an LLM. It watches the chat's SQLite database for
what you say, sends it to any OpenAI-compatible chat endpoint (OpenAI,
llama.cpp, ollama, vLLM, …) together with a system prompt and the
conversation so far, and streams the reply back into the database a
sentence at a time, so scribe starts speaking before the reply is done.
When you talk over a reply, agent stops it and the model is told on the
next turn where you cut in.

```sh
agent talk          # in one terminal: answers talk.db
scribe --chat talk  # in another: talk to it
```

## Configuration

| Option                | Environment                                     | Default                                         |
|-----------------------|-------------------------------------------------|-------------------------------------------------|
| `NAME`                |                                                 | The conversation, `NAME.db` (`.db` optional).   |
| `--prompt TEXT`       | `VOX_AGENT_PROMPT`                              | A short prompt for spoken conversation: plain sentences, no markdown, usually brief. |
| `--prompt-file PATH`  | `VOX_AGENT_PROMPT_FILE`                         |                                                 |
| `--url URL`           | `VOX_AGENT_URL`                                 | `http://127.0.0.1:9931/v1` (a local server)     |
| `--model NAME`        | `VOX_AGENT_MODEL`, then `VOX_SCRIBE_LLM_MODEL`  | required                                        |
| API key               | `VOX_AGENT_KEY`; `OPENAI_API_KEY` too when the URL is OpenAI's | none (fine for local servers)    |
| `--history N`         |                                                 | 40 earlier messages sent with each new one      |

`RUST_LOG=debug` shows more of what it's doing; by default it logs each
message and reply sentence to stderr.

Your words arrive through speech-to-text, so the default prompt asks the
model to read past recognition errors, and since the reply is read
aloud, to avoid markdown, lists, emoji and URLs. Keep that in mind when
writing your own prompt.
