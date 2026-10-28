// The room as this tab sees it: one WebSocket carrying the room's events
// (JSON) and this tab's audio (binary), and the state every component
// reads. The server owns the state; this only applies its events.

import { RoomAudio } from './audio.js';

export const room = $state({
  connected: false,
  denied: false,
  you: null,
  agent: { name: 'AI', state: 'listening', strip: '@agent' },
  mode: 'named',
  people: [],
  strips: {},
  levels: {},
  log: [],
  notice: null,
  audio: 'off', // off | starting | on | error
  audioError: null,
  mic: false,
  muted: false,
  volume: 1,
  // Playing through speakers, not a headset: the browser's echo
  // cancellation keeps the room's sound out of the mic.
  noHeadset: readNoHeadset(),
  // Peak level of what this tab hears, for its meter.
  hearing: 0,
});

function readNoHeadset() {
  try { return localStorage.getItem('vox_room.no_headset') === 'on'; } catch { return false; }
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

/// Who this browser is, from its invite cookie; false when it has none.
export async function whoami() {
  const r = await fetch('/api/me');
  if (r.status === 401) {
    room.denied = true;
    return false;
  }
  room.you = (await r.json()).name;
  return true;
}

export function command(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
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
      room.people = ev.people;
      room.strips = ev.strips;
      room.log = ev.log;
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
      break;
    case 'agent':
      room.agent.state = ev.state;
      break;
    case 'mode':
      room.mode = ev.mode;
      break;
    case 'strips':
      room.strips = ev.strips;
      break;
    case 'levels':
      room.levels = ev.levels;
      break;
    case 'notice':
      room.notice = ev.text;
      clearTimeout(noticeTimer);
      noticeTimer = setTimeout(() => (room.notice = null), 8000);
      break;
  }
}

/// Join the audio: hear the room, and open the mic.
export async function joinAudio() {
  if (audio) return;
  room.audio = 'starting';
  room.audioError = null;
  try {
    audio = new RoomAudio((bytes) => {
      if (ws?.readyState === WebSocket.OPEN) ws.send(bytes);
    });
    await audio.start(room.volume);
    room.audio = 'on';
    hearTimer = setInterval(() => (room.hearing = audio?.takeHeard() ?? 0), 100);
    await audio.startMic({ echoCancellation: room.noHeadset });
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

export async function leaveAudio() {
  command({ type: 'mic', on: false });
  room.mic = false;
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

/// Switch the browser's echo cancellation on (speakers) or off (headset),
/// reopening the mic if it's open.
export async function setNoHeadset(on) {
  room.noHeadset = on;
  try { localStorage.setItem('vox_room.no_headset', on ? 'on' : 'off'); } catch {}
  if (audio?.stream) {
    audio.stopMic();
    try {
      await audio.startMic({ echoCancellation: on });
    } catch (e) {
      room.audioError = e?.message || String(e);
    }
  }
}

export function setVolume(v) {
  room.volume = v;
  audio?.setVolume(v);
}
