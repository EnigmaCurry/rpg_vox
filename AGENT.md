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

Start `agent` first: it asks the model for a word before it opens the
conversation, and scribe won't open a chat that nothing is answering.

## At startup

`agent` checks it can actually do the job before taking the
conversation, and exits 1 if it can't:

* It asks the endpoint for a few words (one `chat/completions` request,
  not streamed), so a wrong URL, a missing or wrong API key, a server
  that isn't running and a model it hasn't got are all reported here —
  with the server's own message — instead of in the middle of a
  conversation, where all you would hear is "Sorry, I couldn't get an
  answer just now." It waits up to a minute, so a local server that
  loads the model on the first request still passes.
* Then it says it is listening, in the database's `responder` setting,
  and keeps saying so every couple of seconds. That is what scribe
  looks for when it opens a chat, so a responder of your own should
  either do the same (`vox_chat::beat`) or be opened with
  `scribe --chat NAME --no-responder`.

## Configuration

The LLM settings are the same environment variables scribe's `--llm`
pass uses, so both talk to the same service.

| Option                | Environment                                     | Default                                         |
|-----------------------|-------------------------------------------------|-------------------------------------------------|
| `NAME`                |                                                 | The conversation, `NAME.db` (`.db` optional).   |
| `--prompt TEXT`       | `VOX_AGENT_PROMPT`                              | A short prompt for spoken conversation: plain sentences, no markdown, usually brief. |
| `--prompt-file PATH`  | `VOX_AGENT_PROMPT_FILE`                         |                                                 |
| `--url URL`           | `VOX_SCRIBE_LLM_URL`                            | `http://127.0.0.1:9931/v1` (a local server)     |
| `--model NAME`        | `VOX_SCRIBE_LLM_MODEL`                          | required                                        |
| API key               | `VOX_SCRIBE_LLM_KEY`; `OPENAI_API_KEY` too when the URL is OpenAI's | none (fine for local servers) |
| `--think`             | `VOX_AGENT_THINK`                               | off: reasoning models answer without thinking first |
| `--history N`         |                                                 | 40 earlier messages sent with each new one      |

`RUST_LOG=debug` shows more of what it's doing; by default it logs each
message and reply sentence to stderr.

Your words arrive through speech-to-text, so the default prompt asks the
model to read past recognition errors, and since the reply is read
aloud, to avoid markdown, lists, emoji and URLs. Keep that in mind when
writing your own prompt.
