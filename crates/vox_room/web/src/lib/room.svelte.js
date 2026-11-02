// The room as this tab sees it: one WebSocket carrying the room's events
// (JSON) and this tab's audio (binary), and the state every component
// reads. The server owns the state; this only applies its events.
//
// The server holds more than one room, and `enterRoom` moves this tab
// between them on the socket it already has: the audio never stops, and
// what arrives back is a whole new `hello`, because everything below
// belonged to the room just left.

import { RoomAudio, listDevices, onDeviceChange, permitMic } from './audio.js';
import {
  connect as connectScreen,
  disconnect as disconnectScreen,
  moved as screenMoved,
} from './screen.svelte.js';

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
  // The name on an unspent invite link this browser is carrying that
  // isn't ours -- one picked up and never used. It does not change who
  // this browser is: it is said at the door so that a name nobody here
  // recognises is explained rather than quietly adopted, and logging out
  // is what hands the room over to it.
  holding: null,
  you: null,
  // The name on your invite. Whoever runs the room chose it and nobody
  // can change it: it is what you are revoked by, and what a room of
  // your own is yours by. `you` is what the room calls you, which is
  // the only name anybody reads -- and the only one you can change.
  invited: null,
  // Why the room wouldn't take a name: one somebody else answers to,
  // or the AI's. Its own, because the box asking is on the door for
  // somebody coming in and on the settings page for everybody else.
  nameError: null,
  // Which room this tab is in, from `hello`: its id (what a command
  // names), what it is called on screen, and whether it is somebody's
  // own -- a room nobody else can come into.
  where: '',
  title: 'Voice room',
  alone: false,
  // The rooms you may go to, and who is in each:
  // { id, title, path, private, here: [names], game }. Yours first,
  // then the house's. Worked out per person by the server, so nobody's
  // own room is on anybody else's list. `path` is where the room
  // lives: the server settles the scheme, this only follows it.
  rooms: [],
  // The AIs in this room: { name, strip, state, mode, engaged }. When
  // each answers is its own, not the room's — except that a game's rule
  // binds all of them, which is what `locked` says.
  agents: [{ name: 'AI', state: 'listening', strip: '@agent', mode: 'named', engaged: false }],

  // The interpreters this room has, when it has any:
  // { level, languages }. `level` is where the one speaking your
  // language reports itself — you hear that one and no other.
  // Null in a room that never interprets, which is a room of your own
  // and any room where everyone shares a language.
  interpreter: null,
  // How loud the interpreter is for *you*. Not the room's: an
  // interpretation is heard by whoever is listening in that language
  // and by nobody else, so turning it down is a thing you do to your
  // own headphones. Kept in this browser and told to the server on
  // every connection and every change of room, the way the mic is.
  myInterpreter: {
    gain: readNumber('vox_room.interpreter', 1),
    mute: readPref('vox_room.interpreter_mute'),
  },
  // How the AI takes its turn. `mode` is what the room is actually doing;
  // `engaged` is true while a back-and-forth (started by saying its name)
  // has it answering every pause, and `locked` while the game being
  // played requires the mode and the buttons can't change it.
  // A game's rule binds every bot in the room while it is played, and
  // then nobody's Answers control is theirs to change.
  locked: false,
  // Whether what people say out loud is put into the other languages as
  // well as the AI's replies. False means a line spoken in a language
  // you don't read is shown as it was said, because no translation of
  // it is coming.
  interprets: false,
  // Who is in this room, and nobody else: { name, mic, muted, speaking,
  // language }. Somebody invited but in another room is not on it --
  // they are on that room's count in `rooms`.
  people: [],
  // What language everybody the room knows speaks, here or not, by
  // name. A lookup table rather than a roster: a line in the
  // conversation outlives its author's standing in the room, and
  // whether it needs translating for you depends on what it was said
  // in. Empty in a room with one language, which translates nothing.
  speaks: {},
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
  // The conversation the room is having: { id, title, at }. `title`
  // empty means nobody has named it, and then what it is called is
  // worked out here — a date is a thing you spell in the reader's own
  // language and clock, so the room keeps the numbers and says nothing
  // about how they read.
  conversation: { id: 0, title: '', at: 0 },
  // The game being played (null: no game, just the room's own
  // conversation), and every saved game.
  game: null,
  games: [],
  // The room's past conversations, once you have asked for them:
  // [{ id, at, game, lines, opening, live }]. Null until you do.
  // Fetched rather than pushed, because it is your own screen: see
  // `reading`.
  history: null,
  // A conversation of this room's you are reading, or null for the one
  // the room is having now. Yours alone — an old conversation can't be
  // spoken into, so opening one changes nothing for anybody else, and a
  // room where one person opening the archive pulled everyone out of
  // the live conversation would be a room nobody dared open it in.
  reading: null,
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
  // You have agreed to be heard, written down and passed on. Kept in
  // this browser, because it is this browser's microphone that is about
  // to be opened; given once, and taken back by unticking it. Without
  // it there is no joining — see `joinAudio`.
  consented: readPref('vox_room.consent'),
  // The settings page is open: push to talk, your volume, and what the
  // browser gave the room to listen with. A view and not a setting, so
  // it is not written down -- a tab opens in the room.
  settings: false,
  // The people page is open, which is the owner's and nobody else's.
  managing: false,
  // Whether this browser owns the house: it invites people and hands
  // out the rooms. What it shows is all it decides -- the server asks
  // again for itself on every one of those.
  owner: false,
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

function readNumber(key, fallback) {
  try {
    // Nothing stored is not a number: `Number(null)` is 0, which for a
    // fader means a browser that has never been here starts with the
    // thing turned off.
    const raw = localStorage.getItem(key);
    if (raw === null || raw.trim() === '') return fallback;
    const n = Number(raw);
    return Number.isFinite(n) && n >= 0 ? n : fallback;
  } catch {
    return fallback;
  }
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
  if (wanted) return;
  wanted = true;
  connect();
  connectScreen();
}

/// Whether the sockets are supposed to be up. A socket that dropped
/// comes back; one that was shut on the way out to the door does not.
let wanted = false;

function connect() {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  // Where to sit. A tab whose socket dropped was in a room when it
  // did, and the room let go of it along with the connection -- so it
  // says where it had got to, rather than being put in the lobby and
  // having to ask to be moved back. Asking to be moved is a change of
  // rooms, and that ends a desktop it was sharing: a moment of bad
  // wifi is not somebody walking out.
  //
  // The server looks the room up as one of theirs like any other, so
  // there is nothing here to be trusted or not.
  const seat = room.where ? `?room=${encodeURIComponent(room.where)}` : '';
  ws = new WebSocket(`${scheme}://${location.host}/room.ws${seat}`);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    room.connected = true;
    retry = 0;
    if (room.mic) command({ type: 'mic', on: true });
    command({ type: 'ptt', on: room.pushToTalk });
    command({ type: 'mute', on: room.muted });
    command({ type: 'interpreter', ...room.myInterpreter });
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') apply(JSON.parse(ev.data));
    else audio?.frame(ev.data);
  };
  ws.onclose = () => {
    room.connected = false;
    // Shut on purpose: don't crawl back.
    if (!wanted) return;
    setTimeout(connect, Math.min(500 * 2 ** retry++, 5000));
  };
}

/// Come back on a fresh socket, now rather than after the backoff a
/// dropped one waits out. A socket is told whose it is when it opens
/// and nothing afterwards changes its mind, so taking a new name needs
/// a new one.
///
/// The audio doesn't stop for it: the mic frames go out on whatever
/// socket is current, the speakers and the microphone are the browser's
/// and untouched, and the `hello` that comes back says who you are now.
function reconnect() {
  if (!wanted) return;
  const old = ws;
  ws = null;
  if (old) {
    // Ours to replace, so the drop handler must not also crawl back:
    // two sockets would both be in the room, under the same name.
    old.onclose = null;
    old.close();
  }
  connect();
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
    room.invited = d.name ?? null;
    room.holding = d.holding ?? null;
    // There is no socket at the door for somebody the room knows
    // either, so the list of languages comes from here as well -- and
    // the one the room has against their invite, which beats what this
    // browser would have guessed.
    room.languages = d.languages ?? [];
    room.lang = myLanguage(room.languages, d.language ?? '');
    room.owner = d.owner ?? false;
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
    // No socket yet, so nothing has been said about them: this browser's
    // memory, or the system, settles it.
    room.lang = myLanguage(room.languages, '');
  } else if (d.state === 'expired') room.expired = true;
  else room.denied = true;
  return false;
}

/// The name the room should call you: at the door while the invite link
/// is still unspent, and from the settings page once you are in. False
/// (with `room.nameError` set) when the room wouldn't have it.
///
/// In the room this is a rename, and nothing here applies it: the room
/// says so to everybody at once, this tab included, and the `renamed`
/// event below is where it lands. The name on your invite is not this
/// and never moves.
export async function setName(display) {
  room.nameError = null;
  const now = room.door ? room.door.display : room.you;
  if (display.trim() === (now ?? '')) return true;
  try {
    const r = await fetch('/api/door/name', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ display }),
    });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.error || r.statusText);
    // At the door there is no socket to say it on, so what the room
    // took is taken from the answer.
    if (room.door) room.door.display = d.display;
    return true;
  } catch (e) {
    room.nameError = e.message || String(e);
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
    // The sockets are `joinAudio`'s to open, and it is the only caller.
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

const roomPath = () => `/api/rooms/${encodeURIComponent(room.where)}/history`;

/// Call a conversation something: the one the room is having, or one
/// you have opened out of its history. Everyone in the room sees it,
/// and the history keeps it. Empty puts it back to being unnamed, which
/// is a date rather than a blank.
export function nameConversation(conversation, title) {
  const was = (room.reading?.id === conversation ? room.reading.title : room.conversation.title) ?? '';
  if (title.trim() === was) return;
  command({ type: 'name_conversation', conversation, title });
}

/// What an unnamed conversation is called: when the first thing in it
/// was said, and failing that the day it was started. The names people
/// give things are better, which is why this is only the default — but
/// a list of "Conversation, Conversation, Conversation" is no list at
/// all, so there is always something here.
// -- the house's people, which is the owner's business -----------------
//
// Nothing here decides anything: the server refuses anybody who isn't
// the owner, and a page that hid these would still not be the thing
// keeping them shut.

/// Everybody invited, what they may do and where they may go. Null when
/// the room won't say -- which is what it says to anybody but the owner.
export async function loadPeople() {
  const r = await fetch('/api/people');
  if (!r.ok) return null;
  return await r.json();
}

/// Invite somebody; returns their link, or throws with what the room
/// said about the name.
export async function invitePerson(name) {
  return await asked('/api/people', { method: 'POST', body: { name } });
}

/// A fresh link for somebody already invited. The browsers they are in
/// keep working: this is the way back in after a link goes astray, not
/// a way to put anybody out.
export async function relinkPerson(name) {
  return await asked(`/api/people/${encodeURIComponent(name)}/link`, { method: 'POST' });
}

/// Remove somebody: their link, their browsers and their rooms.
export async function uninvitePerson(name) {
  return await asked(`/api/people/${encodeURIComponent(name)}`, { method: 'DELETE' });
}

/// Make a room; returns its id.
export async function makeRoom(title) {
  return await asked('/api/house/rooms', { method: 'POST', body: { title } });
}

/// Call a room something else.
export async function renameRoom(id, title) {
  return await asked(`/api/house/rooms/${encodeURIComponent(id)}`, {
    method: 'POST',
    body: { title },
  });
}

/// Take a room out of the house.
export async function deleteRoom(id) {
  return await asked(`/api/house/rooms/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

/// Give somebody a room, or take it away.
export async function setPersonRoom(name, roomId, allowed) {
  return await asked(`/api/people/${encodeURIComponent(name)}/rooms`, {
    method: 'POST',
    body: { room: roomId, allowed },
  });
}

/// One of the above, with what the room said when it refused.
async function asked(url, { method, body }) {
  const r = await fetch(url, {
    method,
    ...(body ? { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : {}),
  });
  const d = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(d.error || r.statusText);
  return d;
}

/// What to call the AIs in a sentence: one name, or every name with
/// "or" before the last. The prose about "the AI" is written for one of
/// them, and a room can have two.
export function theAI(join = 'or') {
  const names = room.agents.map((a) => a.name);
  if (names.length === 0) return 'the AI';
  if (names.length === 1) return names[0];
  return `${names.slice(0, -1).join(', ')} ${join} ${names[names.length - 1]}`;
}

/// Whichever AI is answering right now, for the things that are about
/// the reply in flight -- the Stop button, and the row that says one of
/// them is holding out for a pause.
export function busyAI() {
  return room.agents.find((a) => a.state === 'thinking' || a.state === 'speaking');
}

/// Whichever is waiting for a pause to answer in.
export function waitingAI() {
  return room.agents.find((a) => a.state === 'waiting');
}

export function whenItWas({ said = 0, at = 0 } = {}) {
  const ms = said || at;
  if (!ms) return 'Just now';
  const d = new Date(ms);
  return said
    ? d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
    : d.toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' });
}

/// Every conversation this room has had, newest first.
export async function loadHistory() {
  try {
    const r = await fetch(roomPath());
    if (!r.ok) throw new Error(await r.text());
    room.history = (await r.json()).conversations;
  } catch (e) {
    console.error('[room] history', e);
    notify(`Can't read this room's history: ${e.message || e}`, true);
  }
}

/// Open one of them, to read. The live one is not read: it is the one
/// already on screen, and going "back" to it is leaving this.
export async function readConversation(id) {
  if (room.history?.find((c) => c.id === id)?.live) return backToNow();
  try {
    const r = await fetch(`${roomPath()}/${id}`);
    if (!r.ok) throw new Error(await r.text());
    room.reading = await r.json();
  } catch (e) {
    console.error('[room] read', e);
    notify(`Can't open that conversation: ${e.message || e}`, true);
  }
}

/// Back to the one the room is having now.
export function backToNow() {
  room.reading = null;
}

/// Delete one for good, for everybody. The only thing here that takes
/// something away from the room rather than from your own screen, so
/// it asks first.
export async function forgetConversation(id) {
  if (!(await ask(
    'Delete this conversation for good? Everything said in it, and every card dealt in it, goes — for everyone, and it cannot be undone.',
    'Delete',
  ))) return;
  try {
    const r = await fetch(`${roomPath()}/${id}/delete`, { method: 'POST' });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.error || r.statusText);
    room.history = d.conversations;
    if (room.reading?.id === id) backToNow();
  } catch (e) {
    console.error('[room] delete', e);
    notify(`Can't delete that conversation: ${e.message || e}`, true);
  }
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

// Where a tab is, is a thing it can be sent: every room has an address,
// and the bar follows you from one to another.
//
// The server settles the scheme and says each room's address in the
// directory, so there is one place that knows it and the browser only
// matches strings. Two directions, and they must not chase each other:
// `followUrl` reads the bar and moves the tab, `syncUrl` writes the bar
// once the move has happened. Both are no-ops when the two already
// agree, which is what keeps them from looping.
let pushNext = false;
let urlFollowed = false;

const roomAt = (path) => room.rooms.find((r) => r.path === path);
const pathOf = (id) => room.rooms.find((r) => r.id === id)?.path;

/// Go where the address bar says, if that isn't where we already are.
/// True when a move was asked for, and then the bar is left alone until
/// the room answers.
function followUrl({ push = false } = {}) {
  const want = roomAt(location.pathname);
  if (!want || want.id === room.where) return false;
  pushNext = push;
  command({ type: 'enter_room', id: want.id });
  return true;
}

/// Put the room we are actually in into the bar. An address that names
/// no room -- a stale link, a typo -- is quietly replaced by the one we
/// ended up in rather than left there lying.
function syncUrl() {
  const path = pathOf(room.where);
  if (!path || path === location.pathname) return;
  history[pushNext ? 'pushState' : 'replaceState']({ room: room.where }, '', path);
  pushNext = false;
}

/// Back and forward move between rooms, since that is what the bar
/// changing means. No new entry for a move the history itself asked for.
addEventListener('popstate', () => followUrl());

/// Go to another room. The socket stays where it is: the audio keeps
/// flowing and the server sends back the room it has put you in.
export function enterRoom(id) {
  if (!id || id === room.where) return;
  // Pressing a room is somewhere you can come back from.
  pushNext = true;
  command({ type: 'enter_room', id });
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

/// What language the system is set to, spelled the way the room spells
/// it ("Russian"), when the room speaks it — and the room's own when it
/// doesn't. `Intl` does the naming, so there is no table of codes to
/// keep in step with the room's list.
function browserLanguage(offered) {
  const own = offered[0] ?? '';
  try {
    const tag = (navigator.language || '').split('-')[0];
    if (!tag) return own;
    const named = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag);
    return offered.find((l) => l.toLowerCase() === named?.toLowerCase()) ?? own;
  } catch {
    return own;
  }
}

/// The language to show as yours, out of the ones this room offers.
/// Always one of them, never blank: the door has a box with your
/// language in it, and an empty box is not an answer to the question it
/// is asking.
///
/// What the room says you are wins; failing that, what this browser
/// remembers; failing that — the first time only — whatever the system
/// is set to. Whatever it settles on is written down here, so the
/// system's language is consulted once and can never afterwards
/// overrule something you picked.
function myLanguage(offered, known) {
  if (!offered.length) return known || '';
  const offers = (l) => offered.find((o) => o.toLowerCase() === (l || '').toLowerCase());
  const lang = offers(known) || offers(readChoice('vox_room.lang')) || browserLanguage(offered);
  writeChoice('vox_room.lang', lang);
  return lang;
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
    case 'hello': {
      // A hello arrives on the way in, after a change of room, and when
      // this tab fell too far behind to be patched up. Any of the three
      // means: forget what you had, this is the room.
      const moved = ev.room && ev.room.id !== room.where;
      room.where = ev.room?.id ?? '';
      room.title = ev.room?.title ?? 'Voice room';
      room.alone = ev.room?.private ?? false;
      room.you = ev.you;
      room.agents = ev.agents ?? [];
      room.interpreter = ev.interpreter ?? null;
      room.locked = ev.locked ?? false;
      room.interprets = ev.interprets ?? false;
      room.people = ev.people;
      room.speaks = ev.speaks ?? {};
      room.strips = ev.strips;
      room.log = ev.log;
      // A tab that has just arrived is told the room as it stands, and
      // playback is not part of that: the next report brings it up to
      // date within the frame.
      room.heard = {};
      room.game = ev.game ?? null;
      room.conversation = ev.conversation ?? { id: 0, title: '', at: 0 };
      room.games = ev.games ?? [];
      room.cards = ev.cards ?? [];
      room.languages = ev.languages ?? [];
      // The room is the authority on which language is yours, so once it
      // has spoken, follow it rather than this browser's memory. Nobody
      // the room has never seen has chosen one, so start them off at
      // their browser's language -- shown at the door, and not saved
      // until they come in.
      room.lang = myLanguage(room.languages, languageOf(ev.people, ev.you));
      // A seat in a new room knows nothing of this tab's mic: the
      // server gave it a fresh one, open and unmuted, so say again
      // what it is really doing. Harmless on the way in, where it is
      // what the socket said the moment it opened.
      if (moved) {
        command({ type: 'mic', on: room.mic });
        command({ type: 'ptt', on: room.pushToTalk });
        command({ type: 'mute', on: room.muted });
        command({ type: 'interpreter', ...room.myInterpreter });
        // Another room's history is not this one's, and you arrive in
        // the conversation it is having now.
        room.history = null;
        room.reading = null;
      }
      // Whatever was being shared belonged to the room just left.
      if (moved) screenMoved();
      // The directory may not have arrived yet, the first time; `rooms`
      // does this too, and whichever comes second is the one that acts.
      syncUrl();
      break;
    }
    case 'renamed': {
      // What the room calls somebody has changed, and it calls them
      // that everywhere: the roster, the fader, the meter, and every
      // line they ever said. The room has already moved the lines it
      // keeps, and moves them here rather than sending the
      // conversation again.
      const { was, now } = ev;
      const move = (o) => {
        if (o && was in o) {
          o[now] = o[was];
          delete o[was];
        }
      };
      for (const e of room.log) if (e.who === was) e.who = now;
      // An old conversation open on this screen is read under the name
      // on the roster now, which is the whole of what a display name
      // is: there is no record here of what somebody used to be called.
      if (room.reading) for (const e of room.reading.log) if (e.who === was) e.who = now;
      for (const p of room.people) if (p.name === was) p.name = now;
      for (const c of room.cards) if (c.to === was) c.to = now;
      move(room.strips);
      move(room.levels);
      move(room.speaks);
      if (room.you === was) {
        room.you = now;
        // Our own socket is still in the room under the old name.
        reconnect();
      }
      break;
    }
    case 'rooms':
      room.owner = ev.owner ?? false;
      room.rooms = ev.rooms;
      // The address the tab was opened at is where it wants to be, and
      // this is the first moment there is a list to look it up in. Once
      // only: after that the bar follows the room, not the other way
      // about.
      if (!urlFollowed) {
        urlFollowed = true;
        if (followUrl()) break;
      }
      syncUrl();
      break;
    case 'room':
      // Renamed under you, by the owner.
      room.title = ev.title;
      break;
    case 'people':
      room.people = ev.people;
      room.speaks = ev.speaks ?? {};
      room.lang = myLanguage(room.languages, languageOf(ev.people, room.you));
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
      // The room has moved on, so the list of what it has had is out
      // of date; it is re-read next time somebody opens it.
      room.history = null;
      room.log = ev.log;
      room.heard = {};
      room.game = ev.game ?? null;
      room.conversation = ev.conversation ?? { id: 0, title: '', at: 0 };
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
      notify(`${ev.from ?? theAI()} ${what}: ${ev.card.title}`);
      break;
    }
    case 'card_removed':
      room.cards = room.cards.filter((c) => c.id !== ev.id);
      break;
    case 'conversation':
      // Named with its number on it, because it needn't be the one this
      // tab has open: somebody else may be naming one out of the
      // history while you are reading the live one.
      if (ev.conversation.id === room.conversation.id) room.conversation = ev.conversation;
      if (room.reading?.id === ev.conversation.id) room.reading.title = ev.conversation.title;
      // Whatever was named, the list is out of date.
      room.history = null;
      break;
    case 'games':
      room.games = ev.games;
      break;
    case 'mode': {
      // One bot's, named: another bot in the room is not this one.
      const bot = room.agents.find((b) => b.name === ev.bot);
      if (bot) {
        bot.mode = ev.mode;
        bot.engaged = ev.engaged ?? false;
      }
      room.locked = ev.locked ?? false;
      break;
    }
    case 'agents':
      for (const said of ev.agents) {
        const bot = room.agents.find((b) => b.name === said.name);
        if (bot) bot.state = said.state;
      }
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
    // The room stores the empty string for its own language; the box
    // this came out of has a name in it, so put the name back.
    room.lang = d.language || room.languages[0] || '';
    writeChoice('vox_room.lang', room.lang);
    return true;
  } catch (e) {
    // Put the select back to what the room still believes.
    room.lang = before;
    room.langError = e.message || String(e);
    return false;
  }
}

/// Agree, or take it back. Taking it back shuts the only way in.
export function setConsent(on) {
  room.consented = on;
  writePref('vox_room.consent', on);
  if (on) room.doorError = null;
}

/// Join the audio: open the sockets, hear the room, and open the mic.
export async function joinAudio() {
  if (audio) return;
  // The button is disabled without it; this is the door itself, and the
  // door is the thing that must not open.
  if (!room.consented) {
    room.doorError = 'Agree to be heard and written down before coming in.';
    return;
  }
  room.audio = 'starting';
  room.audioError = null;
  // First time in on a link: the button is what spends it.
  if (room.door && !(await enterOnInvite())) {
    room.audio = 'off';
    return;
  }
  // Only now: at the door you are not in the room, and a tab that is
  // not in the room has no business on its socket -- not on the roster,
  // not sent the conversation, not counted as being anywhere.
  open();
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
  // Back at the door is out of the room, and out is out: the sockets go
  // with the audio. Nobody at the door is on a roster, is sent a
  // conversation, or is counted as being anywhere.
  removeEventListener('pointerdown', resumeAudio);
  removeEventListener('keydown', resumeAudio);
  room.mic = false;
  room.micName = null;
  // The settings page is a page in the room, so it goes with the room:
  // coming back in should land you in the conversation, not in the
  // settings you left open.
  room.settings = false;
  clearInterval(hearTimer);
  room.hearing = 0;
  await audio?.stop();
  audio = null;
  room.audio = 'off';
  close();
}

/// Shut both sockets and stay shut. What the room told us goes with
/// them: none of it is true of somebody standing at the door.
function close() {
  disconnectScreen();
  wanted = false;
  const old = ws;
  ws = null;
  old?.close();
  room.connected = false;
  room.people = [];
  room.log = [];
  room.history = null;
  room.reading = null;
  room.heard = {};
  room.cards = [];
  room.rooms = [];
  room.levels = {};
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

/// How loud you want the interpreter. Yours alone: nobody else's mix
/// changes, and nothing of the room's is written down.
export function setInterpreter({ gain, mute }) {
  if (gain !== undefined) {
    room.myInterpreter.gain = gain;
    writeChoice('vox_room.interpreter', String(gain));
  }
  if (mute !== undefined) {
    room.myInterpreter.mute = mute;
    writePref('vox_room.interpreter_mute', mute);
  }
  command({ type: 'interpreter', ...room.myInterpreter });
}

export function setVolume(v) {
  room.volume = v;
  audio?.setVolume(v);
}
