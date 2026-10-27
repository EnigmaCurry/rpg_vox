/**
 * Voice conversation for the Pi coding agent: live input from
 * `scribe --jsonl --standby` (protocol in SCRIBE_REFERENCE.md) and replies
 * read aloud by pi-tts.py. See PI.md. One scribe runs for the whole Pi
 * session, so its models load once; the mic is only open while you
 * dictate.
 *
 * Usage: pi --extension pi/scribe-voice.ts   (or `just pi`)
 *
 * /voice turns voice mode on and starts listening (it is remembered, as are the voice and
 * style, in ~/.pi/agent/scribe-voice.json); /text turns it off for plain
 * typing and unloads both models. In voice mode Pi starts listening, hands-free:
 * - Talk: once you stop for a moment (SCRIBE_AUTO_FINISH_MS) what you
 *   said is sent, and when the reply is done the mic opens again for a
 *   follow-up.
 * - Space: pause the mic; Space again resumes.
 * - Enter: send now.
 * - Backspace: delete the last dictated word (scribe keeps it out even
 *   when it re-decodes the sentence).
 * - Typing edits the text; scribe is sent the edited text and new speech
 *   is appended after it. Once you have typed something, Space and
 *   Backspace type as usual until the text is sent or cleared.
 * - Escape: drop what is still being transcribed and pause the mic.
 *
 * Replies are read aloud by pi-tts.py (Qwen3-TTS through mlx-audio), a
 * sentence at a time as they stream in, and shown only as far as they
 * have been said; code blocks and tables are not read. Space interrupts
 * the reply and opens the mic for what you say next; Escape interrupts it
 * and pauses. An interrupted reply is kept only as far as it was said.
 * /voice prints the settings and the voices; /voice NAME [how to speak]
 * switches voice (from the next sentence), e.g. `/voice ryan calm and
 * warm, a little playful`. The style stays until changed; `/voice NAME -`
 * speaks plainly again.
 *
 * Environment:
 * - SCRIBE_BIN: the scribe binary (default `scribe` on PATH).
 * - SCRIBE_ARGS: extra arguments, split on spaces (e.g. `--paragraph-gap 1`).
 * - SCRIBE_AUTO_FINISH_MS: quiet that sends what you said (default 1500).
 * - SCRIBE_FINISH_TIMEOUT_MS: how long Enter waits for session_end (default 8000).
 * - PI_TTS: 0 starts in text mode (until /voice is used once).
 * - SCRIBE_VOICE_SETTINGS: where the mode, voice and style are kept.
 * - PI_TTS_PYTHON: the Python with mlx-audio (default pi/.venv, made by
 *   `just pi-tts-venv`, else ~/tts-spike/.venv).
 * - PI_TTS_MODEL: see pi-tts.py. PI_TTS_VOICE / PI_TTS_INSTRUCT are the
 *   defaults before /voice has picked any.
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { homedir } from "node:os";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { CustomEditor, type ExtensionAPI, type ExtensionContext } from "@earendil-works/pi-coding-agent";
import { matchesKey } from "@earendil-works/pi-tui";

const STATUS_KEY = "scribe-voice";
const FINISH_TIMEOUT_MS = Number(process.env.SCRIBE_FINISH_TIMEOUT_MS ?? 8000);
const AUTO_FINISH_MS = Number(process.env.SCRIBE_AUTO_FINISH_MS ?? 1500);

type VoiceEvent = {
	type: string;
	text?: string;
	message?: string;
	level?: "info" | "warning" | "error";
	reason?: string;
	fatal?: boolean;
	model?: string;
	request_id?: string;
};

/**
 * The scribe process, kept running between dictations. Events go to the
 * turn that is listening, if any.
 */
class Scribe {
	private child: ChildProcessWithoutNullStreams;
	/** The dictation scribe's events belong to. */
	turn: VoiceSession | undefined;
	/** scribe is in a turn until its session_end, even after Pi let go. */
	private busy: Promise<void> = Promise.resolve();
	private idle: (() => void) | undefined;
	exited = false;
	private stderrTail: string[] = [];

	constructor(private onExit: (error: string) => void) {
		const bin = process.env.SCRIBE_BIN ?? "scribe";
		const extra = (process.env.SCRIBE_ARGS ?? "").split(/\s+/).filter(Boolean);
		this.child = spawn(bin, ["--jsonl", "--standby", ...extra], { stdio: ["pipe", "pipe", "pipe"] });
		createInterface({ input: this.child.stdout }).on("line", (line) => this.handleLine(line));
		// Logs: keep the last few for an error message, never draw them over the TUI.
		createInterface({ input: this.child.stderr }).on("line", (line) => {
			this.stderrTail.push(line);
			if (this.stderrTail.length > 20) this.stderrTail.shift();
		});
		this.child.on("error", (err) => this.exit(`could not start ${bin}: ${err.message}`));
		this.child.on("exit", (code) => {
			const why = this.stderrTail.filter((l) => l.trim()).slice(-3).join("\n");
			this.exit(`scribe exited (${code})${why ? `: ${why}` : ""}`);
		});
	}

	private exit(error: string): void {
		if (this.exited) return;
		this.exited = true;
		this.idle?.();
		this.turn?.end("error", error);
		if (!this.stopping) this.onExit(error);
	}

	/** stop() was asked for: exiting is expected. */
	private stopping = false;

	private handleLine(line: string): void {
		let event: VoiceEvent;
		try {
			event = JSON.parse(line);
		} catch {
			return;
		}
		if (event.type === "session_end") {
			this.idle?.();
			this.idle = undefined;
		}
		this.turn?.handle(event);
	}

	send(command: object): void {
		if (this.child.stdin.writable) this.child.stdin.write(`${JSON.stringify(command)}\n`);
	}

	/** Begin a turn for `turn` once the last one has ended on scribe's side. */
	async start(turn: VoiceSession, autoFinishMs?: number, paused = false): Promise<void> {
		await this.busy;
		if (this.turn !== turn || this.exited) return;
		this.busy = new Promise((resolve) => (this.idle = resolve));
		this.send({ type: "start", paused, ...(autoFinishMs ? { auto_finish_ms: autoFinishMs } : {}) });
	}

	stop(): void {
		this.stopping = true;
		this.send({ type: "stop" });
		setTimeout(() => {
			if (this.child.exitCode === null) this.child.kill("SIGTERM");
		}, 1000);
	}
}

/** One dictation: what scribe has committed and what it is still guessing. */
class VoiceSession {
	/** Editor text from before dictation started, kept in front of it. */
	prefix: string;
	committed = "";
	partial = "";
	ended = false;
	/** A set_text not yet acknowledged: events before the ack are stale. */
	private pendingSet: string | undefined;
	private endWaiters: Array<(reason: string) => void> = [];

	constructor(
		private scribe: Scribe,
		prefix: string,
		autoFinishMs: number | undefined,
		paused: boolean,
		private onUpdate: () => void,
		private onStatus: (text: string | undefined, level?: "info" | "warning" | "error") => void,
		private onEnd: (reason: string, error?: string) => void,
	) {
		this.prefix = prefix;
		scribe.turn = this;
		this.paused = paused;
		void scribe.start(this, autoFinishMs, paused);
	}

	/** What the editor shows: earlier text, committed words, then the guess. */
	text(): string {
		return this.prefix + this.committed + this.partial;
	}

	handle(event: VoiceEvent): void {
		if (this.ended) return;
		if (this.pendingSet) {
			if (event.type === "text_set" && event.request_id === this.pendingSet) {
				this.pendingSet = undefined;
			}
			// About the text from before the edit.
			if (["partial", "final", "retract", "text_set"].includes(event.type)) return;
		}
		switch (event.type) {
			case "status":
				// scribe's own words for the state the mic is in.
				if (event.message === "Listening") this.onStatus("🎤 listening…");
				else if (event.message === "Paused") this.onStatus("⏸ mic paused: Space to talk");
				else if (event.message !== "Standing by")
					this.onStatus(event.message && `🎤 ${event.message.toLowerCase()}…`, event.level);
				break;
			case "partial":
				this.partial = event.text ?? "";
				this.onUpdate();
				break;
			case "final":
				this.committed += event.text ?? "";
				this.partial = "";
				this.onUpdate();
				break;
			case "retract": {
				// scribe took back the end of the committed text.
				const gone = event.text ?? "";
				if (gone && this.committed.endsWith(gone)) {
					this.committed = this.committed.slice(0, -gone.length);
				}
				this.onUpdate();
				break;
			}
			case "vad_start":
				this.onStatus("🎤 hearing you…");
				break;
			case "vad_end":
				this.onStatus("🎤 listening");
				break;
			case "error":
				if (event.fatal) this.end("error", event.message);
				else this.onStatus(event.message, "warning");
				break;
			case "session_end":
				this.end(event.reason ?? "finished");
				break;
		}
	}

	end(reason: string, error?: string): void {
		if (this.ended) return;
		this.ended = true;
		if (this.scribe.turn === this) this.scribe.turn = undefined;
		for (const w of this.endWaiters.splice(0)) w(reason);
		this.onEnd(reason, error);
	}

	/** Ask for the last words and wait for them (or give up after a while). */
	finish(): Promise<string> {
		if (this.ended) return Promise.resolve("finished");
		let timer: ReturnType<typeof setTimeout> | undefined;
		const done = new Promise<string>((resolve) => this.endWaiters.push(resolve));
		this.scribe.send({ type: "finish", request_id: `finish-${Date.now()}` });
		const timeout = new Promise<string>((resolve) => {
			timer = setTimeout(() => {
				// Gone quiet: give up on the last words, keep scribe.
				this.scribe.send({ type: "cancel" });
				this.end("timeout");
				resolve("timeout");
			}, FINISH_TIMEOUT_MS);
		});
		return Promise.race([done, timeout]).finally(() => clearTimeout(timer));
	}

	/**
	 * Delete the last word. Words scribe has sent are its to delete (it
	 * would send them back otherwise); before those, the editor's own text.
	 */
	deleteWord(): void {
		if (this.committed || this.partial) {
			this.scribe.send({ type: "delete_word" });
			return;
		}
		this.prefix = this.prefix.replace(/\S+\s*$/, "");
		this.onUpdate();
	}

	/** The text was edited by hand: it is all of it now, for scribe too. */
	setText(text: string): void {
		this.prefix = "";
		this.committed = text;
		this.partial = "";
		this.pendingSet = `set-${Date.now()}`;
		this.scribe.send({ type: "set_text", text, request_id: this.pendingSet });
	}

	/** The mic is paused (Space): released until `resume`. */
	paused = false;

	pause(): void {
		this.paused = true;
		this.scribe.send({ type: "pause" });
	}

	resume(): void {
		this.paused = false;
		this.scribe.send({ type: "resume" });
	}

	cancel(): void {
		this.partial = "";
		this.scribe.send({ type: "cancel", request_id: `cancel-${Date.now()}` });
		this.end("cancelled");
	}
}

type VoiceReply = {
	type: string;
	voice?: string;
	voices?: Array<{ name: string; native: string | null }>;
	current?: string;
	instruct?: string;
};

type PlaybackEvent = {
	type: string;
	id?: number;
	played_s?: number;
	total_s?: number | null;
};

/** pi-tts.py, kept running once started: sentences in, audio out. */
class Speaker {
	private child: ChildProcessWithoutNullStreams;
	exited = false;
	private sent = 0;
	private played = 0;
	private waiters: Array<() => void> = [];
	private stderrTail: string[] = [];

	constructor(private onError: (error: string) => void) {
		const local = join(import.meta.dirname, ".venv/bin/python");
		const python =
			process.env.PI_TTS_PYTHON ??
			(existsSync(local) ? local : join(homedir(), "tts-spike/.venv/bin/python"));
		const script = join(import.meta.dirname, "pi-tts.py");
		const env = { ...process.env, PI_TTS_VOICE: settings.voice, PI_TTS_INSTRUCT: settings.instruct };
		this.child = spawn(python, ["-I", script], { stdio: ["pipe", "pipe", "pipe"], env });
		createInterface({ input: this.child.stdout }).on("line", (line) => this.handleLine(line));
		createInterface({ input: this.child.stderr }).on("line", (line) => {
			this.stderrTail.push(line);
			if (this.stderrTail.length > 20) this.stderrTail.shift();
		});
		this.child.on("error", (err) => this.exit(`could not start ${python}: ${err.message}`));
		this.child.on("exit", (code) => {
			const why = this.stderrTail.filter((l) => l.trim()).slice(-3).join("\n");
			this.exit(`pi-tts exited (${code})${why ? `: ${why}` : ""}`);
		});
	}

	/** Sentences sent that haven't finished playing. */
	get busy(): boolean {
		return this.played < this.sent;
	}

	private exit(error: string): void {
		if (this.exited) return;
		this.exited = true;
		this.played = this.sent;
		for (const w of this.waiters.splice(0)) w();
		if (!this.quitting) this.onError(error);
	}

	/** quit() was asked for: exiting is expected. */
	private quitting = false;

	private voiceWaiters: Array<(reply: VoiceReply | undefined) => void> = [];

	/** Ask pi-tts something about voices; undefined if it errs or doesn't answer. */
	private ask(command: object): Promise<VoiceReply | undefined> {
		return new Promise((resolve) => {
			const timer = setTimeout(() => {
				this.voiceWaiters = this.voiceWaiters.filter((w) => w !== done);
				resolve(undefined);
			}, 30000);
			const done = (r: VoiceReply | undefined) => {
				clearTimeout(timer);
				resolve(r);
			};
			this.voiceWaiters.push(done);
			this.send(command);
		});
	}

	voices(): Promise<VoiceReply | undefined> {
		return this.ask({ type: "voices" });
	}

	/** Switch voice; with `instruct`, also how to deliver it ("" for plainly). */
	setVoice(voice: string, instruct?: string): Promise<VoiceReply | undefined> {
		return this.ask({ type: "set_voice", voice, ...(instruct !== undefined ? { instruct } : {}) });
	}

	/** Playback of each sentence: progress while it plays, then said. */
	onPlayback: ((event: PlaybackEvent) => void) | undefined;

	private handleLine(line: string): void {
		let event: VoiceEvent & PlaybackEvent & { last_id?: number | null };
		try {
			event = JSON.parse(line);
		} catch {
			return;
		}
		if (event.type === "idle" && typeof event.last_id === "number") {
			this.played = Math.max(this.played, event.last_id);
			if (!this.busy) for (const w of this.waiters.splice(0)) w();
		} else if (event.type === "voices" || event.type === "voice") {
			const w = this.voiceWaiters.shift();
			w?.(event as unknown as VoiceReply);
		} else if (event.type === "progress" || event.type === "said") {
			this.onPlayback?.(event);
		} else if (event.type === "error") {
			const w = this.voiceWaiters.shift();
			if (w) w(undefined);
			this.onError(event.message ?? "speech failed");
		}
	}

	private send(command: object): void {
		if (this.child.stdin.writable) this.child.stdin.write(`${JSON.stringify(command)}\n`);
	}

	/** Queue a sentence; returns its id for the playback events. */
	say(text: string): number {
		this.sent += 1;
		this.send({ type: "say", text, id: this.sent });
		return this.sent;
	}

	/** Go quiet now and forget what was queued. */
	stop(): void {
		if (this.busy) this.send({ type: "stop" });
	}

	/** Resolves once everything sent so far has been spoken (or dropped). */
	whenIdle(): Promise<void> {
		if (!this.busy) return Promise.resolve();
		return new Promise((resolve) => this.waiters.push(resolve));
	}

	quit(): void {
		this.quitting = true;
		this.send({ type: "quit" });
		setTimeout(() => {
			if (this.child.exitCode === null) this.child.kill("SIGTERM");
		}, 1000);
	}
}

/** Markdown as it would be read aloud: no symbols, links by name. */
function speakable(line: string): string {
	return line
		.replace(/^\s*(#{1,6}|>|[-*+]|\d+[.)])\s+/, "")
		.replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
		.replace(/https?:\/\/\S+/g, "a link")
		.replace(/`([^`]*)`/g, "$1")
		.replace(/(\*\*|__|~~)(.+?)\1/g, "$2")
		.replace(/(^|\W)[*_](\S(?:.*?\S)?)[*_](?=\W|$)/g, "$1$2")
		.replace(/\s+/g, " ")
		.trim();
}

/** A sentence sent to be spoken, and where it came from in the reply. */
type Sentence = {
	id: number;
	/** Its span of the reply's markdown. */
	start: number;
	end: number;
	/** Characters spoken, for guessing its length before it is known. */
	chars: number;
	played: number;
	total: number | undefined;
	said: boolean;
};

/** Speech is about 15 characters a second; a first guess at a sentence's length. */
const CHARS_PER_S = 15;
/** Text shows this far ahead of the voice. */
const LEAD_S = 0.4;

/**
 * One assistant reply as it is spoken: turns the streaming markdown into
 * sentences (whole lines, or a finished sentence of the line still
 * arriving; code blocks and tables are left out), and tracks how far the
 * voice has got, so only what has been said is shown.
 */
class Reply {
	full = "";
	/** Markdown up to here has been turned into sentences or skipped. */
	private pos = 0;
	private inFence = false;
	private streamDone = false;
	sentences: Sentence[] = [];
	/** Esc (or talking over it) cut the reply here. */
	interrupted: number | undefined;
	/** Shown in full without waiting for the voice: speech went away. */
	private released = false;
	private waiters: Array<() => void> = [];

	constructor(private speak: (text: string) => number | undefined) {}

	feed(delta: string): void {
		this.full += delta;
		this.drain(false);
	}

	end(): void {
		this.drain(true);
		this.streamDone = true;
		this.check();
	}

	/** The voice has stopped or gone: show the rest without it. */
	release(): void {
		this.released = true;
		this.check();
	}

	/** Cut the reply at what has been said so far. */
	interrupt(): void {
		if (this.finished) return;
		this.interrupted = this.cut(0);
		this.check();
	}

	playback(e: PlaybackEvent): boolean {
		const s = this.sentences.find((x) => x.id === e.id);
		if (!s) return false;
		if (e.type === "said") s.said = true;
		else {
			s.played = e.played_s ?? s.played;
			s.total = e.total_s ?? s.total;
		}
		this.check();
		return true;
	}

	/** Pacing is over: everything said, or cut, or released. */
	get finished(): boolean {
		return (
			this.released ||
			this.interrupted !== undefined ||
			(this.streamDone && this.sentences.every((s) => s.said))
		);
	}

	whenFinished(): Promise<void> {
		if (this.finished) return Promise.resolve();
		return new Promise((resolve) => this.waiters.push(resolve));
	}

	private check(): void {
		if (this.finished) for (const w of this.waiters.splice(0)) w();
	}

	/** How much of the markdown has been said (plus `lead` seconds ahead). */
	cut(lead = LEAD_S): number {
		if (this.interrupted !== undefined) return this.interrupted;
		if (this.finished) return this.full.length;
		const s = this.sentences.find((x) => !x.said);
		// Nothing waiting to be said: show what was skipped (code, say).
		if (!s) return this.pos;
		if (s.played <= 0) return s.start;
		const length = s.total ?? Math.max(s.chars / CHARS_PER_S, s.played + 0.5);
		const frac = Math.min(1, (s.played + lead) / length);
		let at = s.start + Math.round(frac * (s.end - s.start));
		// Whole words only.
		const span = this.full.slice(0, s.end);
		while (at < s.end && !/\s/.test(span[at] ?? " ")) at++;
		return at;
	}

	private drain(final: boolean): void {
		for (;;) {
			const rest = this.full.slice(this.pos);
			const nl = rest.indexOf("\n");
			if (nl >= 0) {
				this.line(this.pos, this.pos + nl + 1);
				continue;
			}
			if (final) {
				if (rest) this.line(this.pos, this.full.length);
				return;
			}
			// A line still arriving: speak the sentences it has finished.
			const head = rest.trimStart();
			if (this.inFence || head.startsWith("`") || head.startsWith("|")) return;
			const last = [...rest.matchAll(/[.!?…]["')\]]*\s+/g)].at(-1);
			if (!last || last.index === undefined) return;
			this.line(this.pos, this.pos + last.index + last[0].length);
			return;
		}
	}

	private line(start: number, end: number): void {
		this.pos = end;
		const t = this.full.slice(start, end).trim();
		if (t.startsWith("```") || t.startsWith("~~~")) {
			this.inFence = !this.inFence;
			return;
		}
		if (this.inFence || t.startsWith("|")) return;
		const text = speakable(t);
		if (!/[\p{L}\p{N}]/u.test(text)) return;
		const id = this.speak(text);
		if (id === undefined) return;
		this.sentences.push({ id, start, end, chars: text.length, played: 0, total: undefined, said: false });
	}
}

/** The reply being spoken now, if any. */
let reply: Reply | undefined;

/** Stop speaking and cut the reply at what was said: an interruption. */
function interruptSpeech(): void {
	reply?.interrupt();
	speaker?.stop();
	redraw();
}

/** Show the reply again at its current pace. */
function redraw(): void {
	editor?.redraw();
}

/** What /voice and /text set, kept for the next Pi session. */
type Settings = { mode: "voice" | "text"; voice: string; instruct: string };

const SETTINGS_PATH = process.env.SCRIBE_VOICE_SETTINGS ?? join(homedir(), ".pi/agent/scribe-voice.json");

function loadSettings(): Settings {
	const defaults: Settings = {
		mode: process.env.PI_TTS === "0" ? "text" : "voice",
		voice: (process.env.PI_TTS_VOICE ?? "aiden").toLowerCase(),
		instruct: process.env.PI_TTS_INSTRUCT ?? "",
	};
	try {
		return { ...defaults, ...JSON.parse(readFileSync(SETTINGS_PATH, "utf8")) };
	} catch {
		return defaults;
	}
}

function saveSettings(): void {
	try {
		mkdirSync(dirname(SETTINGS_PATH), { recursive: true });
		writeFileSync(SETTINGS_PATH, `${JSON.stringify(settings, null, 2)}\n`);
	} catch {
		// Not remembered next time; nothing else depends on it.
	}
}

let settings = loadSettings();

/** Voice mode: the mic (scribe) and reading replies aloud (pi-tts). */
function voiceOn(): boolean {
	return settings.mode === "voice";
}

/** Qwen3-TTS CustomVoice's speakers and their native languages. */
const VOICES: Array<{ name: string; native: string }> = [
	{ name: "aiden", native: "English" },
	{ name: "ryan", native: "English" },
	{ name: "serena", native: "Chinese" },
	{ name: "vivian", native: "Chinese" },
	{ name: "uncle_fu", native: "Chinese" },
	{ name: "dylan", native: "Chinese, Beijing dialect" },
	{ name: "eric", native: "Chinese, Sichuan dialect" },
	{ name: "ono_anna", native: "Japanese" },
	{ name: "sohee", native: "Korean" },
];

let speaker: Speaker | undefined;

/** The running speaker, started if it isn't. */
function getSpeaker(ctx: ExtensionContext): Speaker {
	if (!speaker || speaker.exited) {
		const s: Speaker = new Speaker((error) => {
			if (s.exited && speaker === s) {
				speaker = undefined;
				reply?.release();
				redraw();
			}
			ctx.ui.notify(`Speech: ${error}`, "error");
		});
		// Reveal the reply as the voice gets through it.
		let shown = -1;
		s.onPlayback = (e) => {
			if (!reply?.playback(e)) return;
			const cut = reply.cut();
			if (cut !== shown) {
				shown = cut;
				redraw();
			}
		};
		speaker = s;
	}
	return speaker;
}

let scribe: Scribe | undefined;

/** The running scribe, started if it isn't. */
function getScribe(ctx: ExtensionContext): Scribe {
	if (!scribe || scribe.exited) {
		const s: Scribe = new Scribe((error) => {
			if (scribe === s) scribe = undefined;
			if (!s.turn) ctx.ui.notify(`Voice input: ${error}`, "error");
		});
		scribe = s;
	}
	return scribe;
}

class VoiceEditor extends CustomEditor {
	voice: VoiceSession | undefined;
	/** Waiting for the last words after Enter. */
	private finishing = false;
	private ctx: ExtensionContext | undefined;
	/** Sent: listen again once the reply is done. */
	private awaitingReply = false;
	/** Typed something: Space and Backspace type as usual until it's sent or cleared. */
	private typed = false;
	/** scribe failed: stop reopening it until Pi is restarted. */
	private broken = false;

	/** Draw everything again (the reply's markdown is paced by the voice). */
	redraw(): void {
		this.tui.invalidate();
		this.tui.requestRender();
	}

	/** The resting state: a dictation with the mic paused, waiting for Space. */
	standby(ctx: ExtensionContext): void {
		this.ctx = ctx;
		if (voiceOn() && !this.voice && !this.awaitingReply && !this.broken) this.listen(ctx, true);
	}

	/** /voice: listen now (trying scribe again if it had failed). */
	enable(ctx: ExtensionContext): void {
		this.broken = false;
		this.awaitingReply = false;
		this.ctx = ctx;
		const v = this.voice;
		if (!v) this.listen(ctx, false);
		else if (v.paused) {
			v.resume();
			this.showStatus();
		}
	}

	/** Text mode: drop the dictation (keeping its text) and type as usual. */
	disable(): void {
		const v = this.voice;
		this.awaitingReply = false;
		this.typed = false;
		if (!v) return;
		this.voice = undefined;
		v.cancel();
		this.setText((v.prefix + v.committed).trimEnd());
		this.ctx?.ui.setStatus(STATUS_KEY, undefined);
		this.tui.requestRender();
	}

	/** Start a hands-free dictation, with the mic open or paused. */
	listen(ctx: ExtensionContext, paused: boolean): void {
		if (this.voice || this.broken) return;
		this.ctx = ctx;
		this.typed = false;
		const before = this.getText();
		const prefix = before && !/\s$/.test(before) ? `${before} ` : before;
		const status = (text: string | undefined, level?: string) => {
			const theme = ctx.ui.theme;
			const color = level === "error" ? "error" : level === "warning" ? "warning" : "accent";
			ctx.ui.setStatus(STATUS_KEY, text ? theme.fg(color, text) : undefined);
		};
		const session: VoiceSession = new VoiceSession(
			getScribe(ctx),
			prefix,
			AUTO_FINISH_MS,
			paused,
			() => this.show(session),
			status,
			(reason, error) => {
				const mine = this.voice === session;
				if (mine) this.voice = undefined;
				ctx.ui.setStatus(STATUS_KEY, undefined);
				if (error) {
					this.broken = true;
					ctx.ui.notify(`Voice input: ${error}`, "error");
				}
				// scribe heard you stop: send it.
				if (reason === "silence" && mine && !this.finishing) {
					this.setText(session.text().trimEnd());
					if (this.getText().trim()) this.submit();
					else this.standby(ctx);
				}
				this.tui.requestRender();
			},
		);
		this.voice = session;
		this.showStatus();
		this.show(session);
	}

	private showStatus(): void {
		const ctx = this.ctx;
		if (!ctx || !this.voice) return;
		const theme = ctx.ui.theme;
		ctx.ui.setStatus(
			STATUS_KEY,
			this.voice.paused ? theme.fg("dim", "⏸ mic paused: Space to talk") : theme.fg("accent", "🎤 listening…"),
		);
	}

	private show(session: VoiceSession): void {
		if (this.voice !== session && !this.finishing) return;
		this.setText(session.text());
		this.tui.requestRender();
	}

	/** Enter: finish the dictation and send it. */
	async stop(): Promise<void> {
		const session = this.voice;
		if (!session || this.finishing) return;
		this.finishing = true;
		this.ctx?.ui.setStatus(STATUS_KEY, this.ctx.ui.theme.fg("accent", "🎤 finishing…"));
		try {
			const reason = await session.finish();
			// A timeout keeps the best guess so far rather than losing it.
			if (reason === "timeout") session.committed += session.partial;
			this.setText(session.text().trimEnd());
		} finally {
			this.finishing = false;
			this.voice = undefined;
			this.ctx?.ui.setStatus(STATUS_KEY, undefined);
		}
		if (this.getText().trim()) this.submit();
		else if (this.ctx) this.standby(this.ctx);
		this.tui.requestRender();
	}

	/** Send the text, and come back for a follow-up after the reply. */
	private submit(): void {
		const command = this.getText().trimStart().startsWith("/");
		this.typed = false;
		super.handleInput("\r");
		// A slash command (or anything else that starts no reply): pause
		// the mic again rather than wait for a reply that won't come.
		const ctx = this.ctx;
		if (command) {
			if (ctx) setTimeout(() => this.standby(ctx), 0);
			return;
		}
		this.awaitingReply = true;
		setTimeout(() => {
			if (this.awaitingReply && !agentRunning && ctx) {
				this.awaitingReply = false;
				this.standby(ctx);
			}
		}, 1500);
		const theme = this.ctx?.ui.theme;
		this.ctx?.ui.setStatus(STATUS_KEY, theme?.fg("dim", "🎤 listening again after the reply (Space: now)"));
	}

	/** The reply is done: listen for the follow-up, or pause if it was stopped. */
	replyDone(ctx: ExtensionContext, aborted: boolean): void {
		this.awaitingReply = false;
		if (this.voice || !voiceOn()) return;
		if (aborted) {
			this.standby(ctx);
			return;
		}
		// Not while the reply is still being read out: the mic would hear it.
		const listen = () => {
			if (!this.voice && !this.awaitingReply) this.listen(ctx, false);
		};
		if (speaker?.busy) void speaker.whenIdle().then(listen);
		else listen();
	}

	handleInput(data: string): void {
		const speaking = speaker?.busy || (reply !== undefined && !reply.finished);
		if (!this.voice) {
			// Space interrupts the reply (or the wait for it): say what's next.
			if (matchesKey(data, "space") && voiceOn() && (speaking || agentRunning) && this.ctx) {
				if (speaking) interruptSpeech();
				this.listen(this.ctx, false);
				return;
			}
			// Escape interrupts the reply too, and Pi stops the agent.
			if (matchesKey(data, "escape") && speaking) interruptSpeech();
			super.handleInput(data);
			return;
		}
		const v = this.voice;
		if (matchesKey(data, "enter")) {
			void this.stop();
			return;
		}
		if (matchesKey(data, "space") && !this.typed) {
			if (v.paused) {
				if (speaking) interruptSpeech();
				this.typed = false;
				v.resume();
			} else v.pause();
			this.showStatus();
			return;
		}
		if (matchesKey(data, "backspace") && !this.typed) {
			v.deleteWord();
			return;
		}
		if (matchesKey(data, "escape")) {
			// Nothing to drop: Escape does what it does in Pi.
			if (v.paused && !v.partial) {
				super.handleInput(data);
				return;
			}
			this.voice = undefined;
			v.cancel();
			this.setText((v.prefix + v.committed).trimEnd());
			if (this.ctx) this.standby(this.ctx);
			this.tui.requestRender();
			return;
		}
		// Anything else edits as usual; an edit becomes the text scribe
		// appends to.
		const before = this.getText();
		super.handleInput(data);
		const after = this.getText();
		if (after === before || this.finishing) return;
		this.typed = after.trim() !== "";
		v.setText(after);
	}
}

let editor: VoiceEditor | undefined;
/** The agent is working on a reply (agent_start until it settles). */
let agentRunning = false;

export default function (pi: ExtensionAPI) {
	pi.on("session_start", (_event, ctx) => {
		if (ctx.mode !== "tui") return;
		// Load the models now, so the first dictation (and reply) starts at once.
		if (voiceOn()) {
			getScribe(ctx);
			getSpeaker(ctx);
		}
		ctx.ui.setEditorComponent((tui, theme, keybindings) => {
			editor = new VoiceEditor(tui, theme, keybindings);
			return editor;
		});
		// In voice mode, listening from the start.
		setTimeout(() => {
			if (voiceOn()) editor?.enable(ctx);
		}, 0);
	});

	// Read replies aloud as they stream in, showing only what has been said.
	pi.on("message_start", (event, ctx) => {
		if (event.message.role !== "assistant" || ctx.mode !== "tui") return;
		reply = undefined;
		if (!voiceOn()) return;
		const s = getSpeaker(ctx);
		reply = new Reply((text) => (voiceOn() && !s.exited ? s.say(text) : undefined));
	});
	pi.on("message_update", (event) => {
		const e = event.assistantMessageEvent;
		if (e.type === "text_delta") reply?.feed(e.delta);
	});
	pi.registerMarkdownTransformer((markdown, context) => {
		const r = reply;
		if (context.messageType !== "assistant" || !r || r.finished && r.interrupted === undefined) return markdown;
		// Only the reply being spoken; Pi draws its text trimmed.
		const full = r.full.trim();
		if (markdown !== full) return markdown;
		const lead = r.full.length - r.full.trimStart().length;
		return markdown.slice(0, Math.max(0, r.cut() - lead)).trimEnd();
	});
	// The message is done when it has been said: the agent goes on (to run
	// tools, say) only then. Cut short, it keeps only what was said, so the
	// model knows where it was interrupted, and drops what it was about to do.
	pi.on("message_end", async (event) => {
		const r = reply;
		if (event.message.role !== "assistant" || !r) return;
		r.end();
		await r.whenFinished();
		redraw();
		if (r.interrupted === undefined) return;
		const said = r.full.slice(0, r.interrupted).trimEnd();
		const content = [
			...event.message.content.filter((c) => c.type === "thinking"),
			...(said ? [{ type: "text" as const, text: `${said} —` }] : []),
		];
		reply = undefined;
		return { message: { ...event.message, content, stopReason: "stop" as const } };
	});

	/** Printed (not spoken): the settings, the voices and how to use them. */
	const describe = (): string => {
		const native = (n: string) => VOICES.find((v) => v.name === n)?.native;
		const current = `${settings.voice}${native(settings.voice) ? ` (${native(settings.voice)})` : ""}`;
		const list = VOICES.map(
			(v) => `  ${v.name === settings.voice ? "▸" : " "} ${v.name.padEnd(9)} ${v.native}`,
		).join("\n");
		return [
			"Voice mode, listening: Space pauses (and talks again), Enter sends, Space or Esc interrupts a reply.",
			`Voice: ${current}   Style: ${settings.instruct || "plain"}`,
			"Voices:",
			list,
			"Usage:",
			"  /voice                 voice mode with these settings",
			"  /voice NAME            switch voice (keeps the style)",
			"  /voice NAME STYLE...   voice and how it speaks: /voice ryan calm and warm, a little playful",
			"  /voice NAME -          voice, plain style",
			"  /text                  back to typing; unloads the voice models",
		].join("\n");
	};

	pi.registerCommand("voice", {
		description: "Voice mode (talk and hear replies); /voice NAME [style] picks the voice",
		getArgumentCompletions: (prefix) => {
			const p = prefix.trim().toLowerCase();
			if (p.includes(" ")) return null;
			const items = VOICES.filter((v) => v.name.startsWith(p)).map((v) => ({
				value: v.name,
				label: v.name,
				description: v.native,
			}));
			return items.length ? items : null;
		},
		handler: async (args, ctx) => {
			const [first, ...rest] = args.trim().split(/\s+/);
			if (first) {
				const name = first.toLowerCase();
				if (!VOICES.some((v) => v.name === name)) {
					ctx.ui.notify(`No voice "${first}".\n${describe()}`, "error");
					return;
				}
				settings.voice = name;
				const style = rest.join(" ");
				// Anything after the name is how to say it; "-" goes back to plain.
				if (style) settings.instruct = style === "-" ? "" : style;
			}
			settings.mode = "voice";
			saveSettings();
			if (ctx.mode === "tui") {
				// A pi-tts already running gets the voice now; a new one starts with it.
				void getSpeaker(ctx).setVoice(settings.voice, settings.instruct);
				getScribe(ctx);
				editor?.enable(ctx);
			}
			ctx.ui.notify(describe(), "info");
		},
	});

	pi.registerCommand("text", {
		description: "Text mode: no mic, replies aren't read aloud; unloads the voice models",
		handler: async (_args, ctx) => {
			settings.mode = "text";
			saveSettings();
			// A reply being read shows in full rather than being cut short.
			reply?.release();
			reply = undefined;
			speaker?.stop();
			speaker?.quit();
			speaker = undefined;
			editor?.disable();
			scribe?.stop();
			scribe = undefined;
			ctx.ui.setStatus(STATUS_KEY, undefined);
			redraw();
			ctx.ui.notify("Text mode: the mic is off and replies aren't read aloud. /voice to talk again.", "info");
		},
	});

	pi.on("agent_start", () => {
		agentRunning = true;
	});

	// Hands-free: the agent has finished answering.
	pi.on("agent_settled", (event, ctx) => {
		agentRunning = false;
		if (ctx.mode === "tui") editor?.replyDone(ctx, event.aborted);
	});

	pi.on("session_shutdown", () => {
		scribe?.stop();
		scribe = undefined;
		speaker?.quit();
		speaker = undefined;
		editor = undefined;
	});
}
