// The room as this tab sees it: one WebSocket carrying the room's events
// (JSON) and this tab's audio (binary), and the state every component
// reads. The server owns the state; this only applies its events.

import { RoomAudio, listDevices, onDeviceChange, permitMic } from './audio.js';

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
  // How the AI takes its turn. `mode` is what the room is actually doing;
  // `engaged` is true while a back-and-forth (started by saying its name)
  // has it answering every pause, and `locked` while the game being
  // played requires the mode and the buttons can't change it.
  mode: 'named',
  engaged: false,
  locked: false,
  people: [],
  strips: {},
  levels: {},
  log: [],
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

export function connect() {
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
  if (d.state === 'invited') room.door = { name: d.name, display: d.display };
  else if (d.state === 'expired') room.expired = true;
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
    connect();
    return true;
  } catch (e) {
    room.doorError = e.message || String(e);
    return false;
  }
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
      room.mode = ev.mode;
      room.engaged = ev.engaged ?? false;
      room.locked = ev.locked ?? false;
      room.people = ev.people;
      room.strips = ev.strips;
      room.log = ev.log;
      room.game = ev.game ?? null;
      room.games = ev.games ?? [];
      room.cards = ev.cards ?? [];
      break;
    case 'people':
      room.people = ev.people;
      break;
    case 'entry':
      upsert(ev.entry);
      break;
    case 'entry_removed':
      room.log = room.log.filter((e) => e.id !== ev.id);
      break;
    case 'reset':
      room.log = ev.log;
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
