// The room as this tab sees it: one WebSocket carrying the room's events
// (JSON) and this tab's audio (binary), and the state every component
// reads. The server owns the state; this only applies its events.

import { RoomAudio, listDevices, onDeviceChange, permitMic } from './audio.js';
import { connect as connectScreen, stopSharing } from './screen.svelte.js';

export const room = $state({
  connected: false,
  // No invite link and no cookie: there is nothing to let this browser in.
  denied: false,
  // A link that has already been traded for a cookie, in a browser that
  // hasn't got one.
  expired: false,
  // The invite at the door, until Join Audio spends it: { name, display }.
  // `name` is the name on the link, which stays the key the room is
  // revoked by; `display` is what they will be called, and is theirs to
  // change here and only here.
  door: null,
  // Why the door wouldn't take something: a name somebody else has, or a
  // link that expired between opening it and pressing the button.
  doorError: null,
  you: null,
  agent: { name: 'AI', state: 'listening', strip: '@agent' },
  // The interpreters, when the room has any: { strip, languages }. One
  // fader for all of them, and pulling it down turns the voices off
  // without turning off the reading.
  interpreter: null,
  // How the AI takes its turn. `mode` is what the room is actually doing;
  // `engaged` is true while a back-and-forth (started by saying its name)
  // has it answering every pause, and `locked` while the game being
  // played requires the mode and the buttons can't change it.
  mode: 'named',
  engaged: false,
  locked: false,
  // Whether what people say out loud is put into the other languages as
  // well as the AI's replies. False means a line spoken in a language
  // you don't read is shown as it was said, because no translation of
  // it is coming.
  interprets: false,
  people: [],
  strips: {},
  levels: {},
  log: [],
  // How far the AI's turn has been heard, in each language it is being
  // said in, by entry id: [{ language, ms, total, done }]. A translation
  // is a different length from the words it translates, so one language
  // can be over while another is still going, and everyone can see which
  // -- there is nothing private about it. Only here while the entry is
  // live; the room says nothing about a turn it has finished.
  heard: {},
  // The game being played (null: the lobby), and every saved game.
  game: null,
  games: [],
  notice: null,
  // A question waiting for the user's answer: { text, yes, resolve }.
  asking: null,
  // A game's prompt this person asked to see: { name, summary, prompt }.
  peek: null,
  // Cards the AI dealt to you alone: { id, to, title, body, at }. The
  // server only ever sends these to your own tabs, and they are the AI's
  // to give, rewrite and take back -- there is nothing here that clears
  // one.
  cards: [],
  audio: 'off', // off | starting | on | error
  audioError: null,
  mic: false,
  // What the browser calls the mic in use, e.g. "Jabra SPEAK 410".
  micName: null,
  // The devices to choose between at the door, and the choices, which
  // are this browser's and are remembered for next time.
  devices: { mics: [], speakers: [] },
  micId: readChoice('vox_room.mic'),
  speakerId: readChoice('vox_room.speaker'),
  // The language you speak and want to hear, also chosen at the door.
  // Unlike the devices this one is the room's business too, so it is
  // saved on the server; the copy here is only what to show until the
  // room says otherwise. Empty means the room's own language.
  lang: readChoice('vox_room.lang'),
  // The languages the door offers, from the room. One of them (or none)
  // means there is nothing to translate and the select is not shown.
  languages: [],
  langError: null,
  // Set when the browser refused the microphone, so the door can say so
  // rather than offering a list of nameless devices.
  devicesError: null,
  muted: false,
  volume: 1,
  // Push to talk: the mic stays muted except while Space (or the talk
  // button) is held.
  pushToTalk: readPref('vox_room.push_to_talk'),
  talking: false,
  // Peak level of what this tab hears, for its meter.
  hearing: 0,
});

function readChoice(key) {
  try { return localStorage.getItem(key) || ''; } catch { return ''; }
}

function writeChoice(key, id) {
  try { localStorage.setItem(key, id || ''); } catch {}
}

function readPref(key) {
  try { return localStorage.getItem(key) === 'on'; } catch { return false; }
}

function writePref(key, on) {
  try { localStorage.setItem(key, on ? 'on' : 'off'); } catch {}
}

let ws = null;
let audio = null;
let retry = 0;
let noticeTimer = null;
let hearTimer = null;

/// Open both of the room's sockets: its events and this tab's audio on
/// /room.ws, and desktop sharing on /screen.ws. Separate connections, so
/// a burst of video can't delay a 20 ms frame of somebody talking.
export function open() {
  connect();
  connectScreen();
}

function connect() {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${scheme}://${location.host}/room.ws`);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    room.connected = true;
    retry = 0;
    if (room.mic) command({ type: 'mic', on: true });
    command({ type: 'ptt', on: room.pushToTalk });
    command({ type: 'mute', on: room.muted });
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') apply(JSON.parse(ev.data));
    else audio?.frame(ev.data);
  };
  ws.onclose = () => {
    room.connected = false;
    setTimeout(connect, Math.min(500 * 2 ** retry++, 5000));
  };
}

/// Who this browser is, from its own cookie, or whose invite link it is
/// holding. True when there is a cookie, and so a room to connect to.
export async function whoami() {
  let d;
  try {
    d = await (await fetch('/api/door')).json();
  } catch (e) {
    // The room isn't answering -- restarting, perhaps. Say nothing about
    // invites: this browser may well be perfectly welcome.
    console.error('[room] door', e);
    return false;
  }
  if (d.state === 'in') {
    room.you = d.display;
    return true;
  }
  // An invite spends nothing until Join Audio: see `enterOnInvite`.
  if (d.state === 'invited') {
    room.door = { name: d.name, display: d.display };
    // There is no socket yet -- it wants a session, and the invite is
    // still unspent -- so the door's own answer is where the languages
    // come from, and a browser the room has never seen starts at its
    // own language.
    room.languages = d.languages ?? [];
    room.lang = room.lang || browserLanguage(room.languages);
  } else if (d.state === 'expired') room.expired = true;
  else room.denied = true;
  return false;
}

/// The name the room should call you, asked at the door while the invite
/// link is still unspent. False (with `room.doorError` set) when the
/// room wouldn't have it.
export async function setName(display) {
  room.doorError = null;
  if (!room.door || display.trim() === room.door.display) return true;
  try {
    const r = await fetch('/api/door/name', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ display }),
    });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.error || r.statusText);
    room.door.display = d.display;
    return true;
  } catch (e) {
    room.doorError = e.message || String(e);
    return false;
  }
}

/// Trade the invite link for this browser's own cookie. The link is
/// spent here, which is why it happens on the way through the door and
/// not when the link was opened.
async function enterOnInvite() {
  try {
    const r = await fetch('/api/door/join', { method: 'POST' });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) {
      // Somebody else got there first, or it was revoked while the card
      // was open: there is nothing left to come in on.
      if (d.state === 'expired') room.expired = true;
      throw new Error(d.error || r.statusText);
    }
    room.you = d.display;
    room.door = null;
    open();
    return true;
  } catch (e) {
    room.doorError = e.message || String(e);
    return false;
  }
}

/// Give this browser's cookie back. The link it came in on is spent, so
/// coming back takes a new one -- which is worth asking about first.
export async function logOut() {
  if (!(await ask(
    'Log out of the room on this browser? Your invite link has been used, so you need a new one to come back.',
    'Log out',
  ))) return;
  try {
    await fetch('/api/door/logout', { method: 'POST' });
  } catch (e) {
    room.doorError = e.message || String(e);
    return;
  }
  // Start over with no cookie rather than unpicking the socket, the
  // audio and the room by hand: the door is the whole page as it is.
  location.reload();
}

export function command(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

/// Ask the user to confirm, in the page. (The browser's own confirm()
/// blocks the page, and with it the room's audio.)
export function ask(text, yes = 'OK') {
  return new Promise((resolve) => {
    room.asking?.resolve(false);
    room.asking = { text, yes, resolve };
  });
}

export function answer(ok) {
  const a = room.asking;
  room.asking = null;
  a?.resolve(ok);
}

/// Fetch a game's hidden prompt, for this person only.
export async function peekGame(name) {
  try {
    const r = await fetch(`/api/games/${encodeURIComponent(name)}/prompt`);
    if (!r.ok) throw new Error(await r.text());
    room.peek = await r.json();
  } catch (e) {
    console.error('[room] peek', e);
    room.notice = { text: `Can't show ${name}'s prompt: ${e.message || e}`, error: true };
  }
}

export async function enterGame(name) {
  if (await ask(`Enter ${name}? Everyone in the room goes into the game, and this chat is cleared.`, `Enter ${name}`)) {
    command({ type: 'enter_game', name });
  }
}

/// Show a message in the footer for a while.
function notify(text, error = false) {
  room.notice = { text, error };
  clearTimeout(noticeTimer);
  if (!error) noticeTimer = setTimeout(() => (room.notice = null), 8000);
}

/// Your own language out of a roster, or undefined when the roster
/// doesn't have you yet (the door, before the link is spent).
function languageOf(people, you) {
  return people?.find((p) => p.name === you)?.language;
}

/// This browser's own language, spelled the way the room spells it
/// ("Russian"), when the room offers it. `Intl` does the naming, so
/// there is no table of codes to keep in step with the room's list.
/// Empty for the room's own language, which is what index 0 is.
function browserLanguage(offered) {
  try {
    const tag = (navigator.language || '').split('-')[0];
    if (!tag || offered.length < 2) return '';
    const named = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag);
    const match = offered.find((l) => l.toLowerCase() === named?.toLowerCase());
    return match && match !== offered[0] ? match : '';
  } catch {
    return '';
  }
}

function upsert(entry) {
  const i = room.log.findIndex((e) => e.id === entry.id);
  if (i >= 0) room.log[i] = entry;
  else {
    room.log.push(entry);
    room.log.sort((a, b) => a.at - b.at);
  }
}

function apply(ev) {
  switch (ev.type) {
    case 'hello':
      room.you = ev.you;
      room.agent = ev.agent;
      room.interpreter = ev.interpreter ?? null;
      room.mode = ev.mode;
      room.engaged = ev.engaged ?? false;
      room.locked = ev.locked ?? false;
      room.interprets = ev.interprets ?? false;
      room.people = ev.people;
      room.strips = ev.strips;
      room.log = ev.log;
      // A tab that has just arrived is told the room as it stands, and
      // playback is not part of that: the next report brings it up to
      // date within the frame.
      room.heard = {};
      room.game = ev.game ?? null;
      room.games = ev.games ?? [];
      room.cards = ev.cards ?? [];
      room.languages = ev.languages ?? [];
      // The room is the authority on which language is yours, so once it
      // has spoken, follow it rather than this browser's memory. Nobody
      // the room has never seen has chosen one, so start them off at
      // their browser's language -- shown at the door, and not saved
      // until they come in.
      room.lang =
        languageOf(ev.people, ev.you) ?? browserLanguage(room.languages);
      break;
    case 'people':
      room.people = ev.people;
      room.lang = languageOf(ev.people, room.you) ?? room.lang;
      break;
    case 'entry':
      upsert(ev.entry);
      break;
    case 'entry_removed':
      room.log = room.log.filter((e) => e.id !== ev.id);
      delete room.heard[ev.id];
      break;
    case 'heard':
      room.heard[ev.id] = ev.of;
      break;
    case 'reset':
      room.log = ev.log;
      room.heard = {};
      room.game = ev.game ?? null;
      // Cards belong to the conversation that dealt them.
      room.cards = ev.cards ?? [];
      break;
    case 'card': {
      // Only this person's tabs are sent it; say so, without the contents.
      // A card the AI rewrote keeps its place in the list.
      const i = room.cards.findIndex((c) => c.id === ev.card.id);
      if (i >= 0) room.cards[i] = ev.card;
      else room.cards.push(ev.card);
      const what = ev.replaced ? 'changed your card' : 'sent you a card';
      notify(`${room.agent.name} ${what}: ${ev.card.title}`);
      break;
    }
    case 'card_removed':
      room.cards = room.cards.filter((c) => c.id !== ev.id);
      break;
    case 'games':
      room.games = ev.games;
      break;
    case 'agent':
      room.agent.state = ev.state;
      break;
    case 'mode':
      room.mode = ev.mode;
      room.engaged = ev.engaged ?? false;
      room.locked = ev.locked ?? false;
      break;
    case 'strips':
      room.strips = ev.strips;
      break;
    case 'levels':
      room.levels = ev.levels;
      break;
    case 'notice':
      // Errors stay until dismissed; anything else fades.
      if (ev.level === 'error') console.error('[room]', ev.text);
      else console.info('[room]', ev.text);
      notify(ev.text, ev.level === 'error');
      break;
  }
}

/// Find the microphones and speakers to choose between. Needs the mic
/// permission first, or the browser gives ids with no names.
export async function loadDevices({ permit = false } = {}) {
  if (permit) room.devicesError = await permitMic();
  room.devices = await listDevices();
  // A device that has been unplugged since last time: back to the
  // system default rather than a choice that no longer exists.
  const keep = (list, id) => (list.some((d) => d.id === id) ? id : '');
  room.micId = keep(room.devices.mics, room.micId);
  room.speakerId = keep(room.devices.speakers, room.speakerId);
  return room.devices;
}

/// Watch for a headset being plugged in or pulled out.
export function watchDevices() {
  return onDeviceChange(() => loadDevices());
}

export function setMicDevice(id) {
  room.micId = id;
  writeChoice('vox_room.mic', id);
}

export function setSpeakerDevice(id) {
  room.speakerId = id;
  writeChoice('vox_room.speaker', id);
  audio?.setSpeaker(id);
}

/// Type something into the room instead of saying it: exactly the
/// characters you meant, which is what a URL or a command wants. The
/// room sanitizes and translates it; all this does is send it, and only
/// once there is something to send.
export function sendText(text) {
  if (!text.trim()) return false;
  command({ type: 'text', text });
  return true;
}

/// The language you speak and want to hear. Saved on the server against
/// your invite, so it follows you to another browser and is there for
/// the room to mix and translate by; remembered here too, so the door
/// shows the right one before the room has said anything.
export async function setLanguage(language) {
  const before = room.lang;
  room.lang = language;
  room.langError = null;
  try {
    const r = await fetch('/api/door/language', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ language }),
    });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.error || r.statusText);
    room.lang = d.language;
    writeChoice('vox_room.lang', d.language);
    return true;
  } catch (e) {
    // Put the select back to what the room still believes.
    room.lang = before;
    room.langError = e.message || String(e);
    return false;
  }
}

/// Join the audio: hear the room, and open the mic.
export async function joinAudio() {
  if (audio) return;
  room.audio = 'starting';
  room.audioError = null;
  // First time in on a link: the button is what spends it.
  if (room.door && !(await enterOnInvite())) {
    room.audio = 'off';
    return;
  }
  try {
    audio = new RoomAudio((bytes) => {
      if (ws?.readyState === WebSocket.OPEN) ws.send(bytes);
    });
    await audio.start(room.volume, room.speakerId);
    // Any click or key brings back audio the browser stopped.
    addEventListener('pointerdown', resumeAudio);
    addEventListener('keydown', resumeAudio);
    room.audio = 'on';
    hearTimer = setInterval(() => (room.hearing = audio?.takeHeard() ?? 0), 100);
    await audio.startMic({ deviceId: room.micId });
    room.micName = await audio.micName();
    if (room.pushToTalk) setMuted(true);
    audio.muted = room.muted;
    room.mic = true;
    command({ type: 'mic', on: true });
  } catch (e) {
    console.error(e);
    room.audioError = e?.message || String(e);
    room.audio = audio?.ctx ? 'on' : 'error';
    if (room.audio === 'error') {
      await audio?.stop();
      audio = null;
    }
  }
}

function resumeAudio() {
  audio?.resume();
}

export async function leaveAudio() {
  // Back at the door is out of the room: a desktop shared from behind it
  // would be one nobody can hear you talk about.
  stopSharing();
  removeEventListener('pointerdown', resumeAudio);
  removeEventListener('keydown', resumeAudio);
  command({ type: 'mic', on: false });
  room.mic = false;
  room.micName = null;
  clearInterval(hearTimer);
  room.hearing = 0;
  await audio?.stop();
  audio = null;
  room.audio = 'off';
}

export function setMuted(m) {
  room.muted = m;
  if (audio) audio.muted = m;
  command({ type: 'mute', on: m });
}

/// Push to talk on: muted until Space or the talk button is held. Off:
/// back to an open mic.
export function setPushToTalk(on) {
  room.pushToTalk = on;
  writePref('vox_room.push_to_talk', on);
  clearTimeout(releaseTimer);
  room.talking = false;
  command({ type: 'ptt', on });
  setMuted(on);
}

// After letting go, the mic stays open a moment so the last word gets
// through.
const RELEASE_MS = 300;
let releaseTimer = null;

export function talk(on) {
  if (!room.pushToTalk || !room.mic) return;
  clearTimeout(releaseTimer);
  if (on) {
    room.talking = true;
    if (room.muted) setMuted(false);
  } else if (room.talking) {
    room.talking = false;
    releaseTimer = setTimeout(() => {
      if (room.pushToTalk && !room.talking) setMuted(true);
    }, RELEASE_MS);
  }
}

export function setVolume(v) {
  room.volume = v;
  audio?.setVolume(v);
}
