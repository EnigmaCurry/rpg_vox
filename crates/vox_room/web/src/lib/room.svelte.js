// The room as this tab sees it: one WebSocket carrying the room's events
// (JSON) and this tab's audio (binary), and the state every component
// reads. The server owns the state; this only applies its events.
//
// The server holds more than one room, and `enterRoom` moves this tab
// between them on the socket it already has: the audio never stops, and
// what arrives back is a whole new `hello`, because everything below
// belonged to the room just left.

import { ARM_MS, chirp, hush } from './chirp.js';
import { RoomAudio, listDevices, onDeviceChange, permitMic } from './audio.js';
import { Link } from './link.js';
import {
  connect as connectScreen,
  disconnect as disconnectScreen,
  moved as screenMoved,
} from './screen.svelte.js';

/// Where an invite link lands. The server serves the app here and does
/// nothing else with it: the token is the page's to hold and to send
/// back, not the server's to write down.
const JOIN = '/join/';

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
  // The voices in this room: { name, strip, state, mode, engaged,
  // stops, puppet }. `mode` is what the room has this one doing — a
  // game's rule, a show, or a voice that has stood itself down — and
  // nothing here sets it: whether an AI may answer *you* is your own
  // BOTS switch, which is `bots` below. `stops` is whether talking
  // over it cuts it off, and that one is the bot's: a game never
  // takes it.
  //
  // `puppet` is a character the first AI speaks through rather than an
  // AI of its own. It has a name, a face and a fader like everybody
  // else and nothing to set: there is nothing behind it to answer
  // with. See ROOM.md, Puppets.
  agents: [
    {
      name: 'AI',
      state: 'listening',
      strip: '@agent',
      mode: 'named',
      engaged: false,
      stops: 'interrupted',
      puppet: false,
    },
  ],

  // The puppet show, if one is on:
  // { mode: 'scripted' | 'adlib', on_stage: [names],
  //   playing: { title, lines, said } | null }.
  //
  // Null the rest of the time, which is nearly always, and that is what
  // says there is no playhouse to draw: no stage, no director's box,
  // and the conversation is simply the room's. A show is on while there
  // is a character standing on the stage — `playing` is the *script*,
  // which is a thing that happens inside a show rather than what one
  // is, and is null between two scenes and all through the questions
  // afterwards.
  show: null,

  // Where everybody is sitting while a show is on:
  // { people: { name: seat }, bots: { name: seat }, director }.
  // Null outside a show. A seat is 'audience', 'box' or 'stage', and
  // which one you are in decides who can hear you — see ROOM.md.
  seats: null,

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
  // The game being played requires a mode of every bot in the room,
  // which is a thing to say in prose and nothing anybody can change.
  locked: false,
  // Whether the AIs may answer *you*: the BOTS button in the footer,
  // beside push to talk. 'any' is the button on, which is what
  // everybody starts as; 'named' and 'never' are the two ways it can
  // be off. The room is the authority -- it is kept against you, in
  // every room of the house and after a restart -- and this is what it
  // last said, so the button reads the truth after a reconnect.
  bots: 'any',
  // Which of those two the off position means, for you. This browser's,
  // like `pttHush`: the nuance belongs with the person holding the
  // button, and the room only ever hears which of the three it is
  // being put into. See `setBotsOff`.
  botsOff: readChoice('vox_room.bots_off') === 'never' ? 'never' : 'named',
  // Whether what people say out loud is put into the other languages as
  // well as the AI's replies. False means a line spoken in a language
  // you don't read is shown as it was said, because no translation of
  // it is coming.
  interprets: false,
  // Who is in this room, and nobody else: { name, mic, muted, speaking,
  // language, bots }. Somebody invited but in another room is not on it --
  // they are on that room's count in `rooms`.
  people: [],
  // What language everybody the room knows speaks, here or not, by
  // name. A lookup table rather than a roster: a line in the
  // conversation outlives its author's standing in the room, and
  // whether it needs translating for you depends on what it was said
  // in. Empty in a room with one language, which translates nothing.
  speaks: {},
  // Who has a face, by the name the room calls them, to the version
  // theirs is at. A lookup table for the same reason as `speaks`: a
  // line outlives its author's standing in the room, and it is still
  // shown with their face. The version is what `avatarOf` puts in the
  // address, so a new picture is a new URL and the old one can be
  // cached forever.
  avatars: {},
  // The pictures made in this conversation, by id: { n, who, prompt,
  // ready, failed }. A picture card in the log looks itself up here --
  // the entry says which picture it is and this says whether there is
  // anything to show yet.
  pictures: {},
  // The voice samples read in this conversation, by id: { n, who,
  // passage, said, ready, secs, failed }. Read by the card in the log
  // the same way a picture's is: the entry says which clip it is and
  // this says whether the room is still listening, what was recorded,
  // and how long it is.
  samples: {},
  // The voices the house has cloned from any of them: { id, name, who,
  // said, created }. Not a control -- which voice an AI speaks in is
  // asked of it out loud -- but the room shows what there is, because
  // a voice of yours kept on a server is a thing you should be able
  // to see the list of.
  voices: [],
  // Whose face is open, or null. One at a time, and nobody else's
  // screen changes: looking at who somebody is is a thing you do.
  profile: null,
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
  // What holding it means for the room's own sound. False -- the
  // default, and what an open mic always is -- is full duplex: you go
  // on hearing everybody while you talk, which is what a headset
  // wants. True is half duplex: the room goes silent for as long as
  // you hold, which is what speakers in front of a microphone want.
  // Yours and this browser's, like the mode it qualifies; see
  // `setTalkHush`.
  pttHush: readPref('vox_room.ptt_hush'),
  // Which of a phone's volume keys is its push-to-talk key, and whether
  // holding it chirps. Not this browser's, unlike the talk key and
  // `pttHush` above: a phone has no keyboard, so the app lends a volume
  // key to the microphone, and every phone has the same two of them --
  // where a desk and a laptop have different keyboards, which is why
  // the room's own talk key is kept per browser. Stored against your
  // invite so a second phone does not have to be told again.
  //
  // Empty means you have never said, and the app on the phone keeps its
  // own answer. Nothing here does anything in a browser: it is read off
  // this page by the Android app, which is the only thing with volume
  // keys to lend. See ROOM_ANDROID.md.
  phoneKey: '',
  // Whether holding the talk key chirps -- 'on', 'off', or empty for
  // the default, which is on. Push to talk's and not the phone's: the
  // on-screen button and the talk key on a keyboard arm and chirp
  // exactly as a phone's volume key does. Yours rather than this
  // browser's, for the same reason as the key above. See `lib/chirp.js`.
  pttChirp: '',
  // Why the last change to those two did not stick, or null. Declared
  // here rather than assigned into the object later: a property that
  // was not there when the state was made is not one anything redraws
  // for.
  pttError: null,
  talking: false,
  // Whether the room's own sound is cut just now, which it is for as
  // long as push to talk is held on half duplex: the mic is open and
  // the speakers are in front of it. Said on the button, because
  // silence that is not explained is a fault.
  hushed: false,
  // Peak level of what this tab hears, for its meter.
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

/// The invite token, when this page was opened on an invite link.
///
/// In memory, for as long as this page is open, and nowhere else. The
/// room is handed nothing and this browser keeps nothing until Join
/// Audio spends it -- so looking at a link and thinking better of it
/// leaves no trace, and closing the tab is all it takes to be rid of
/// one. A browser with no cookie and no link in the bar is a browser
/// the room knows nothing about, which is what being logged out is.
///
/// The address bar is where it lives until then, because that is where
/// whoever followed the link put it: reloading the door still works,
/// and `syncUrl` writes the bar over the moment the link is spent.
let invite = location.pathname.startsWith(JOIN)
  ? decodeURIComponent(location.pathname.slice(JOIN.length))
  : null;

let link = null;
let audio = null;
let noticeTimer = null;

/// Open both of the room's sockets: its events and this tab's audio on
/// /room.ws, and desktop sharing on /screen.ws. Separate connections, so
/// a burst of video can't delay a 20 ms frame of somebody talking.
///
/// The first of them is the worker's, not this thread's -- see link.js.
/// Reconnecting is its too, for the same reason the frames are: the
/// thread that draws the room is the one a phone with its screen off
/// stops giving time to, and a socket waiting to come back wants a
/// clock that is still running.
export function open() {
  if (wanted) return;
  wanted = true;
  // One worker for the page: coming back into the room is a new socket
  // on the thread that is already there.
  link ??= new Link({
    onEvent: apply,
    onUp: () => {
      room.connected = true;
      if (room.mic) command({ type: 'mic', on: true });
      command({ type: 'ptt', on: room.pushToTalk });
      command({ type: 'mute', on: room.muted });
      command({ type: 'interpreter', ...room.myInterpreter });
    },
    onDown: () => (room.connected = false),
  });
  // Where to sit. A tab whose socket dropped was in a room when it
  // did, and the room let go of it along with the connection -- so it
  // says where it had got to, rather than being put in the lobby and
  // having to ask to be moved back. Asking to be moved is a change of
  // rooms, and that ends a desktop it was sharing: a moment of bad
  // wifi is not somebody walking out.
  //
  // The server looks the room up as one of theirs like any other, so
  // there is nothing here to be trusted or not.
  link.connect(room.where);
  connectScreen();
}

/// Whether the sockets are supposed to be up. A socket that dropped
/// comes back; one that was shut on the way out to the door does not.
let wanted = false;

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
  link?.reconnect();
}

/// One of the door's requests: whatever it is asking, the link this
/// page is holding goes with it, because until the link is spent that
/// is the only thing saying who is at the door.
function door(path, body = {}) {
  return fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...body, token: invite }),
  });
}

/// Who this browser is, from its own cookie, or whose invite link it is
/// holding. True when there is a cookie, and so a room to connect to.
export async function whoami() {
  let d;
  try {
    d = await (await door('/api/door')).json();
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
    rememberPhone(d.phoneKey ?? '', d.pttChirp ?? '');
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
    const r = await door('/api/door/name', { display });
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
    const r = await door('/api/door/join');
    const d = await r.json().catch(() => ({}));
    if (!r.ok) {
      // Somebody else got there first, or it was revoked while the card
      // was open: there is nothing left to come in on.
      if (d.state === 'expired') room.expired = true;
      throw new Error(d.error || r.statusText);
    }
    room.you = d.display;
    room.door = null;
    // Spent. What the room gave back for it is a cookie, which is this
    // browser's own and the only thing it has ever been given to keep;
    // the token is of no further use to anybody, and the address bar's
    // copy goes when the first `hello` writes the bar over.
    invite = null;
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

/// Put down an invite without spending it.
///
/// The way out of a link you did not mean to follow. Logging out is no
/// help here and is not offered: holding an unspent invite means there
/// is no cookie to give back, and the only thing tying this browser to
/// that room is the token in the address. So this is the address
/// without it -- which in a browser is the room saying you need a link,
/// and in the Android app is the screen that takes one.
///
/// Nothing is spent and nothing is asked first. The link still works,
/// and whoever it belongs to can still use it.
export function putLinkDown() {
  location.href = '/';
}

export function command(msg) {
  if (link?.up) link.send(msg);
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

/// Put one AI in a room, or take it out. `roomId` is a room's id, or
/// the `own` the house page was given, which is everybody's own room —
/// one answer for all of them, because a room of your own is yours.
export async function setBotRoom(name, roomId, allowed) {
  return await asked(`/api/bots/${encodeURIComponent(name)}/rooms`, {
    method: 'POST',
    body: { room: roomId, allowed },
  });
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

/// The AIs in this room, and not the puppets. Everything that says
/// "the AI" or asks one of them to do something means these: a puppet
/// is a voice in the room with nothing behind it, so a sentence
/// telling you to ask one is a sentence telling you to ask nobody.
export function theAIs() {
  return room.agents.filter((a) => !a.puppet);
}

/// The puppets in the room, in the order they came on stage: the
/// characters the first AI speaks through. The stage is drawn from
/// these, and it is drawn at all only while there are some.
export function thePuppets() {
  return room.agents.filter((a) => a.puppet);
}

/// Where somebody is sitting, by the name the room calls them.
/// 'audience' outside a show, which is where everybody is when there
/// are no seats to be in.
export function seatOf(who) {
  if (!room.seats) return 'audience';
  return room.seats.people?.[who] ?? room.seats.bots?.[who] ?? 'audience';
}

/// Everybody in one part of the playhouse, people and bots together,
/// as { who, bot, state }. The bands are drawn from this, so a face
/// moves from one to another by the room saying so and nothing else.
export function sitting(seat) {
  if (!room.seats) return [];
  const out = [];
  for (const p of room.people) {
    if (seatOf(p.name) === seat) out.push({ who: p.name, bot: false, state: null });
  }
  for (const a of room.agents) {
    if (a.puppet) continue;
    if (seatOf(a.name) === seat) out.push({ who: a.name, bot: true, state: a.state });
  }
  return out;
}

/// Take a seat in one of the three parts, or put somebody else in one.
/// The one seat nobody may move is the director's; the room says so
/// rather than this hiding the control.
export function takeSeat(seat, who = undefined) {
  command({ type: 'seat', seat, ...(who ? { who } : {}) });
}

/// The lines said on the stage in this show, oldest first. They are
/// never in the conversation — a play is not the room talking — so the
/// stage has a log of its own and this is it.
export function stageLines() {
  return room.log.filter((e) => e.kind === 'line');
}

/// What has been said in the director's box in this show, oldest
/// first. While the playhouse is open it is never in the conversation
/// below — the box is the one part of the room the audience's log must
/// not be mingled with, in either direction — so the box has a log of
/// its own and this is it, the way the stage does.
///
/// Only what was *said* in there. Cards, pictures and the programme at
/// the curtain are put in front of the whole room and stay in the
/// conversation, where they can be opened and played.
export function boxLines() {
  if (!room.show || room.reading) return [];
  return room.log.filter((e) => e.seat === 'box' && !!e.text?.trim());
}

/// What to call the AIs in a sentence: one name, or every name with
/// "or" before the last. The prose about "the AI" is written for one of
/// them, and a room can have two.
export function theAI(join = 'or') {
  const names = theAIs().map((a) => a.name);
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
    const got = await r.json();
    // An old conversation brings its own pictures. Added rather than
    // replacing, so coming back out of it leaves the live
    // conversation's cards still able to draw themselves.
    for (const p of got.pictures ?? []) room.pictures[p.id] = p;
    // And its own voice samples, for the same reason.
    for (const c of got.samples ?? []) room.samples[c.id] = c;
    room.reading = got;
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

/// Where to fetch somebody's face, or null when they have none and are
/// to be drawn as the tint their name already has.
///
/// The version the room sent is in the query, which is the whole of how
/// this is cached: a new picture is a new address, and the server marks
/// the answer immutable. So the browser fetches one face once, however
/// many lines that person has said.
export function avatarOf(who) {
  const at = room.avatars[who];
  return at ? `/api/avatar/${encodeURIComponent(who)}?v=${at}` : null;
}

/// Take your own face off. Yours alone -- the server refuses anybody
/// else's, whoever asks. It stays in the history below, so putting it
/// back on is one press.
export async function forgetAvatar() {
  await fetch(`/api/avatar/${encodeURIComponent(room.you)}`, { method: 'DELETE' });
}

/// One of your own past faces, at a version: what Settings shows the
/// row of, and what it puts back on.
export function faceAt(at) {
  return `/api/avatar/${encodeURIComponent(room.you)}?v=${at}`;
}

/// The faces you have worn, newest first. Asked for when Settings
/// opens rather than kept on the roster: ten of anybody's past faces
/// is nobody else's business and not something to send on every
/// change of who is talking.
export async function myFaces() {
  try {
    const r = await fetch(`/api/avatar/${encodeURIComponent(room.you)}/all`);
    if (!r.ok) return [];
    return (await r.json()).faces ?? [];
  } catch {
    return [];
  }
}

/// Put one of them back on. The room tells every tab, so the squares
/// in the conversation change behind the page.
export async function wearFace(at) {
  await fetch(`/api/avatar/${encodeURIComponent(room.you)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ at }),
  });
}

/// Where to fetch a picture of this conversation.
export function pictureOf(id) {
  return `/api/picture/${encodeURIComponent(id)}`;
}

/// Where to fetch a voice sample read in this conversation.
export function sampleOf(id) {
  return `/api/sample/${encodeURIComponent(id)}`;
}

// -- a picture dropped into the room -----------------------------------
//
// Everything that holds a picture here is a PNG and was before any of
// this: the card in the conversation, the face beside somebody's name,
// the square in the modal. So a drop is turned into one here, in the
// tab that has the file, rather than the room learning a second format
// for the sake of what a camera happens to write.
//
// It is scaled down on the way as well. Nothing shows a picture larger
// than the card does, and the server keeps no picture bigger than a
// face may be -- so a photograph straight off a phone is re-encoded to
// something the room can actually use, instead of being refused for
// being what phones make.

/// The longest edge of a dropped picture, in order of preference. The
/// first is a shade over the 192 CSS pixels the card draws it at, so it
/// is sharp on a 2x screen and nothing more; the rest are what to try
/// when a photograph is still too big as a PNG at that size, which
/// lossless compression of a photograph can be.
const PICTURE_EDGES = [384, 256, 160];
/// What the room will keep, which is what a face may be: 1 MB, the
/// `AVATAR_BYTES_MAX` of `room.rs`. The server enforces it -- this is
/// here so that a drop that cannot fit is told so on the spot, rather
/// than after the upload.
const PICTURE_BYTES_MAX = 1024 * 1024;
/// How many pictures one drop may bring in. A drop is one deliberate
/// act and these are cards in a shared conversation; a folder let go
/// of by accident is not a reason to fill the room.
const PICTURE_DROP_MAX = 8;

/// The images in what was dropped, in the order they were dropped.
const imagesIn = (files) => [...files].filter((f) => f.type.startsWith('image/'));

/// The recordings in what was dropped. A voice memo off a phone may
/// arrive with no type at all, so the extension is read as well — the
/// browser is about to try to decode it either way, and a file it
/// cannot decode is refused by the decoder with a better message than
/// a guess from the name would give.
const CLIP_SUFFIX = /\.(wav|mp3|m4a|aac|ogg|oga|opus|flac|webm|weba|aiff?|caf|amr|3gp)$/i;
const clipsIn = (files) =>
  [...files].filter((f) => f.type.startsWith('audio/') || CLIP_SUFFIX.test(f.name || ''));

/// The rate a clip is sent at: Qwen3-TTS's own, which is what the
/// clone server wants and what `SAMPLE_HZ` in `room.rs` keeps a
/// recorded one at. Resampled here by the browser's own audio engine,
/// which does it well and for nothing.
const CLIP_HZ = 24000;
/// The longest clip worth sending, a shade over the thirty seconds the
/// room keeps (`SAMPLE_MAX`). Trimmed rather than refused: somebody
/// dropping a two-minute recording meant the voice in it, and the
/// first half-minute of it is a voice.
const CLIP_SECS_MAX = 30;

/// One audio file as a WAV the room will take: mono, 24 kHz, 16-bit.
///
/// The browser decodes it, which is the whole reason this is done
/// here: it already has a decoder for everything it can play — mp3,
/// m4a, ogg, a voice memo — and the room would need a library for each
/// and would still be guessing at the rest. An `OfflineAudioContext`
/// does the downmix and the resample on the way through, properly,
/// because that is what it is for.
async function wavOf(file) {
  const bytes = await file.arrayBuffer();
  // A plain context first, only to decode: `decodeAudioData` keeps the
  // file's own rate and channel count, and the offline one below is
  // what changes them.
  const Ctx = window.AudioContext || window.webkitAudioContext;
  const decoded = await new Ctx().decodeAudioData(bytes);
  const secs = Math.min(decoded.duration, CLIP_SECS_MAX);
  if (!(secs > 0)) throw new Error('there is no sound in it');
  const frames = Math.floor(secs * CLIP_HZ);
  const off = new OfflineAudioContext(1, frames, CLIP_HZ);
  const src = off.createBufferSource();
  src.buffer = decoded;
  src.connect(off.destination);
  src.start(0, 0, secs);
  const mono = (await off.startRendering()).getChannelData(0);

  // 16-bit mono WAV, which is what the clone server is given and what
  // the room keeps. Written by hand because it is 20 lines and the
  // alternative is a library to emit a header.
  const head = 44;
  const out = new DataView(new ArrayBuffer(head + mono.length * 2));
  const ascii = (at, s) => [...s].forEach((c, i) => out.setUint8(at + i, c.charCodeAt(0)));
  ascii(0, 'RIFF');
  out.setUint32(4, head - 8 + mono.length * 2, true);
  ascii(8, 'WAVEfmt ');
  out.setUint32(16, 16, true); // the size of this fmt chunk
  out.setUint16(20, 1, true); // PCM, uncompressed
  out.setUint16(22, 1, true); // one channel
  out.setUint32(24, CLIP_HZ, true);
  out.setUint32(28, CLIP_HZ * 2, true); // bytes a second
  out.setUint16(32, 2, true); // bytes a frame
  out.setUint16(34, 16, true); // bits a sample
  ascii(36, 'data');
  out.setUint32(40, mono.length * 2, true);
  for (let i = 0; i < mono.length; i++) {
    // Clamped before it is scaled: a sample over 1.0 would wrap to
    // the opposite extreme and be heard as a click.
    const v = Math.max(-1, Math.min(1, mono[i]));
    out.setInt16(head + i * 2, Math.round(v * 32767), true);
  }
  return { wav: new Blob([out.buffer], { type: 'audio/wav' }), secs };
}

/// Put a recording somebody dropped into the conversation, to clone a
/// voice from: a card with the next number on it, playable, and from
/// then on a clip like any other — `save_voice` takes that number.
///
/// Nothing is drawn here, as with a picture: the server tells every tab
/// about the card, including this one. A clip arriving is a thing that
/// happened in the room.
///
/// One at a time and only the first, because a clip is a deliberate
/// thing to bring in and the room keeps thirty seconds of it: a folder
/// of recordings let go of by accident must not become eight cards and
/// eight model decodes.
export async function dropClip(files, how = 'dropped') {
  const clips = clipsIn(files);
  if (!clips.length) return false;
  const file = clips[0];
  if (clips.length > 1) {
    notify(`That is ${clips.length} recordings; the first is coming in.`);
  }
  notify(`Reading ${file.name}…`);
  try {
    const { wav, secs } = await wavOf(file);
    const where = `/api/rooms/${encodeURIComponent(room.where)}/clip`;
    const r = await fetch(`${where}?name=${encodeURIComponent(file.name)}`, {
      method: 'POST',
      headers: { 'content-type': 'audio/wav' },
      body: wav,
    });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.error || r.statusText);
    if (secs >= CLIP_SECS_MAX) {
      notify(`${file.name} is longer than ${CLIP_SECS_MAX} seconds, so the first ${CLIP_SECS_MAX} came in.`);
    }
  } catch (e) {
    console.error('[room] clip', e);
    notify(`Can't bring in ${file.name}: ${e.message || e}`, true);
  }
  return true;
}

/// One file as a PNG the room will keep, or null when it cannot be
/// made small enough. Decoded by the browser, so whatever it can
/// decode can be dropped -- JPEG off a camera, a WebP off the web, a
/// screenshot off the clipboard.
async function pngOf(file) {
  const bitmap = await createImageBitmap(file);
  try {
    for (const edge of PICTURE_EDGES) {
      // Never enlarged: a 64-pixel sprite is dropped in at 64 pixels.
      const scale = Math.min(1, edge / Math.max(bitmap.width, bitmap.height));
      const canvas = document.createElement('canvas');
      canvas.width = Math.max(1, Math.round(bitmap.width * scale));
      canvas.height = Math.max(1, Math.round(bitmap.height * scale));
      const ctx = canvas.getContext('2d');
      ctx.imageSmoothingQuality = 'high';
      ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      const png = await new Promise((done) => canvas.toBlob(done, 'image/png'));
      if (png && png.size <= PICTURE_BYTES_MAX) return png;
    }
    return null;
  } finally {
    bitmap.close();
  }
}

/// Put what was dropped -- or pasted -- into the conversation: each
/// picture becomes a card with the next number on it, which is then a
/// picture like any other, so the AI can be asked for it as somebody's
/// face and the conversation keeps it.
///
/// Nothing is drawn here. The server tells every tab about the card,
/// including this one, the same way it does for a picture the AI made:
/// a picture arriving is a thing that happened in the room, not a
/// thing that happened on this screen.
///
/// `how` is the word for what was just done, for the one message that
/// has to name it: being told "only pictures can be dropped" by a
/// window you pasted into is being answered about somebody else.
export async function dropPicture(files, how = 'dropped') {
  // A recording is the other thing worth bringing in, and it goes
  // somewhere else entirely: into the conversation as a clip to clone
  // a voice from. Checked first, so a dropped voice memo is not
  // answered with "only pictures can be dropped".
  if (await dropClip(files, how)) return;
  const pictures = imagesIn(files);
  if (!pictures.length) {
    notify(`Only pictures and recordings can be ${how} into the room.`);
    return;
  }
  const taking = pictures.slice(0, PICTURE_DROP_MAX);
  if (pictures.length > taking.length) {
    notify(`That is ${pictures.length} pictures; the first ${taking.length} are coming in.`);
  }
  // One at a time, so the cards are numbered in the order they were
  // dropped rather than in the order the uploads happen to finish.
  for (const file of taking) {
    try {
      const png = await pngOf(file);
      if (!png) throw new Error("it can't be made small enough to keep");
      const where = `/api/rooms/${encodeURIComponent(room.where)}/picture`;
      const r = await fetch(`${where}?name=${encodeURIComponent(file.name)}`, {
        method: 'POST',
        headers: { 'content-type': 'image/png' },
        body: png,
      });
      const d = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(d.error || r.statusText);
    } catch (e) {
      console.error('[room] drop', e);
      notify(`Can't bring in ${file.name}: ${e.message || e}`, true);
      return;
    }
  }
}

/// Open somebody's face: who they are, as much as the house knows.
/// Nobody else's screen changes.
export async function openProfile(who) {
  // Shown at once with what is already on screen, so the modal opens
  // on the click rather than on the round trip; the rest fills in.
  room.profile = { who, loading: true };
  await loadProfile(who);
}

async function loadProfile(who) {
  try {
    const r = await fetch(`/api/profile/${encodeURIComponent(who)}`);
    if (!r.ok) throw new Error(String(r.status));
    const got = await r.json();
    // Not if they closed it, or opened somebody else, while this was
    // in the air.
    if (room.profile?.who === who) room.profile = { ...got, loading: false };
  } catch {
    if (room.profile?.who === who) room.profile = { who, loading: false, failed: true };
  }
}

export function closeProfile() {
  room.profile = null;
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

/// Show a message in the footer for a while. Exported as `notice` for
/// the few things said from a component rather than from here: a word
/// about what just happened belongs in the one place the room puts
/// them, however it was reached.
export { notify as notice };
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

/// Whether the AIs may answer you, off the roster the room just sent.
/// The room keeps it against you rather than against this browser, so
/// what it says is what it is -- in this room and the next one.
function botsOf(people, you) {
  return people?.find((p) => p.name === you)?.bots ?? 'any';
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
      // The worker reconnects on its own, so it is the one that has to
      // know which room to ask for.
      link?.setSeat(room.where);
      room.title = ev.room?.title ?? 'Voice room';
      room.alone = ev.room?.private ?? false;
      room.you = ev.you;
      room.agents = ev.agents ?? [];
      room.interpreter = ev.interpreter ?? null;
      room.locked = ev.locked ?? false;
      room.show = ev.show ?? null;
      room.seats = ev.seats ?? null;
      room.interprets = ev.interprets ?? false;
      room.people = ev.people;
      room.speaks = ev.speaks ?? {};
      room.avatars = ev.avatars ?? {};
      room.pictures = Object.fromEntries((ev.pictures ?? []).map((p) => [p.id, p]));
      room.samples = Object.fromEntries((ev.samples ?? []).map((s) => [s.id, s]));
      room.voices = ev.voices ?? [];
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
      room.bots = botsOf(ev.people, ev.you);
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
      // Faces ride along with the roster; see `Room::emit_people`.
      room.avatars = ev.avatars ?? {};
      room.lang = myLanguage(room.languages, languageOf(ev.people, room.you));
      // And so does your BOTS switch, which is what makes the button
      // right again after a reconnect or a change of room.
      room.bots = botsOf(ev.people, room.you);
      break;
    case 'entry':
      upsert(ev.entry);
      break;
    case 'picture':
      // A picture finished, one way or the other. The card is already
      // in the log; this is what turns its spinner into the picture,
      // or into why it never came.
      room.pictures[ev.picture.id] = ev.picture;
      break;
    case 'sample':
      // The room has stopped listening, one way or the other: the card
      // becomes the clip, with what was said and a button to hear it,
      // or says why there is nothing.
      room.samples[ev.sample.id] = ev.sample;
      break;
    case 'voices':
      // One was kept or deleted. The whole list, because it is short
      // and the house's rather than this conversation's.
      room.voices = ev.voices ?? [];
      break;
    case 'voice': {
      // One of the AIs is speaking in a cloned voice, or in its own
      // again. It says so on its strip for as long as it lasts.
      const bot = room.agents.find((b) => b.name === ev.bot);
      if (bot) bot.voice = ev.voice ?? null;
      break;
    }
    case 'bio':
      // Somebody was written about. Only matters if their face is the
      // one open, and then it is read again rather than patched.
      if (room.profile?.who === ev.who) loadProfile(ev.who);
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
      // Cards belong to the conversation that dealt them, and so do
      // pictures: these are the new conversation's, not a patch on
      // the ones that were on screen.
      room.cards = ev.cards ?? [];
      room.pictures = Object.fromEntries((ev.pictures ?? []).map((p) => [p.id, p]));
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
    case 'stops': {
      // One bot's, like `mode`, and with no `locked` about it.
      const bot = room.agents.find((b) => b.name === ev.bot);
      if (bot) bot.stops = ev.stops;
      break;
    }
    case 'agents':
      for (const said of ev.agents) {
        const bot = room.agents.find((b) => b.name === said.name);
        if (bot) bot.state = said.state;
      }
      break;
    case 'agents_here':
      // The owner put an AI in this room or took one out, or a puppet
      // came on or left the stage: a fader appears or goes, with
      // what it is doing and its Stops.
      room.agents = ev.agents ?? [];
      break;
    case 'show':
      // A show opened, got a line further on, or closed.
      room.show = ev.show ?? null;
      break;
    case 'seats':
      // Somebody took a seat — which can change the show itself, since
      // stepping onto the stage ends whatever script was running.
      room.seats = ev.seats ?? null;
      room.show = ev.show ?? null;
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
    const r = await door('/api/door/language', { language });
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

/// What the phone's volume keys do: which one talks and whether holding
/// it chirps. Saved on the server against your invite rather than in
/// this browser, which is the whole point -- it is how a second phone
/// arrives already knowing.
///
/// Nothing in a browser changes. The Android app reads these off this
/// page; see ROOM_ANDROID.md.
/// Keep what the room said where the phone can find it.
///
/// Written into this browser's storage as well as the state, which is
/// the odd part and is the whole mechanism: the Android app has no way
/// to read a Svelte rune, and it already reads the room's talk-key
/// binding out of `localStorage` to know which key to press. So the
/// server's answer is mirrored there and the app picks it up in the
/// poll it already runs. The app reads; it is never told. Nothing is
/// injected into this page and there is no way back out of it into the
/// app, which is the property worth keeping. See ROOM_ANDROID.md.
function rememberPhone(key, chirp) {
  room.phoneKey = key;
  room.pttChirp = chirp;
  writeChoice('vox_room.phone_key', key);
  writeChoice('vox_room.ptt_chirp', chirp);
}

export async function setTalk(key, chirp) {
  const was = [room.phoneKey, room.pttChirp];
  rememberPhone(key, chirp);
  room.pttError = null;
  try {
    const r = await door('/api/talk', { key, chirp });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) throw new Error(d.error || r.statusText);
    rememberPhone(d.phoneKey ?? key, d.pttChirp ?? chirp);
    return true;
  } catch (e) {
    // Back to what the room still believes, so the radio is never
    // showing a choice the phone will not actually be told about.
    rememberPhone(was[0], was[1]);
    room.pttError = e.message || String(e);
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
    audio = new RoomAudio(link);
    await audio.start(room.volume, room.speakerId);
    openInsides();
    // Any click or key brings back audio the browser stopped.
    addEventListener('pointerdown', resumeAudio);
    addEventListener('keydown', resumeAudio);
    room.audio = 'on';
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

/// The room's insides, for the console, while developing.
///
/// `vox.jitter` is the player worklet's reading of its own buffer --
/// how much sound it is holding and how much it wants to be -- which is
/// the one number worth having when the audio is choppy and the only
/// one nothing in the room shows. `vox.link` and `vox.audio` are there
/// to be poked at: the socket and the codec are the worker's, and
/// everything else here is module-scoped and out of reach otherwise.
///
/// `import.meta.env.DEV` is `false` in a built app and the whole of this
/// goes with it, so `just room` serves nothing of it. It is `pnpm dev`
/// on :5173 that has it (see `just room-ui`).
function openInsides() {
  if (!import.meta.env.DEV) return;
  window.vox = { link, audio, jitter: null };
  audio.onStats = (s) => (window.vox.jitter = s);
}

function closeInsides() {
  if (!import.meta.env.DEV) return;
  delete window.vox;
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
  room.hushed = false;
  await audio?.stop();
  audio = null;
  closeInsides();
  room.audio = 'off';
  close();
}

/// Shut both sockets and stay shut. What the room told us goes with
/// them: none of it is true of somebody standing at the door.
function close() {
  disconnectScreen();
  wanted = false;
  link?.shut();
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
/// back to an open mic, which is full duplex -- everybody hears
/// everybody, all the time.
export function setPushToTalk(on) {
  room.pushToTalk = on;
  writePref('vox_room.push_to_talk', on);
  clearTimeout(releaseTimer);
  room.talking = false;
  // Let go of mid-hold -- the switch is beside the button, and turning
  // the mode off while holding it is a thing a hand does. Nothing is
  // listening any more, so nothing is worth being quiet for.
  hushRoom(false);
  command({ type: 'ptt', on });
  setMuted(on);
}

/// Whether the AIs may answer you: the BOTS button in the footer.
///
/// The inversion of a control that used to be the bot's. When an AI
/// answers was one setting per bot, shared by the room and changeable
/// by anybody in it, which made being answered a thing the room
/// decided about you -- and two people at one table want opposite
/// things of the same AI. This is the same question asked of the
/// person it happens to.
///
/// Off is not silence in the room: everybody else goes on being
/// answered, and everything said is in the one conversation whoever
/// said it. What it stops is your words being the thing an AI answers.
///
/// The room is the authority and keeps it against you, in every room
/// of the house and after a restart, so this only asks -- `room.bots`
/// is set from the roster that comes back.
export function setBots(on) {
  const how = on ? 'any' : room.botsOff;
  // Shown at once rather than when the roster lands: the button is a
  // button. A room that refuses it says so and sends the roster, which
  // is what puts this right.
  room.bots = how;
  command({ type: 'bots', how });
}

/// What the off position of that button means, which is the one nuance
/// in it: `named` -- the default -- still lets an AI answer when you
/// call it by its name, and `never` is nothing at all.
///
/// This browser's, like `setTalkHush`: it is a sentence about how you
/// want to be spoken to, and the room only ever hears which of the
/// three states the switch is in. Changed while the button is already
/// off, it moves the switch there now -- the setting would otherwise
/// be a promise about next time.
export function setBotsOff(how) {
  room.botsOff = how === 'never' ? 'never' : 'named';
  writeChoice('vox_room.bots_off', room.botsOff);
  if (room.bots !== 'any') setBots(false);
}

/// What holding the talk button does to the room's own sound.
///
/// Two different rooms want opposite things of the same hold, and only
/// the person holding it knows which room they are in. On a headset,
/// cutting the room is a loss and nothing else: you cannot hear the
/// word you are talking over, so holding the key to interrupt means
/// not hearing the AI stop. On speakers, the room's own sound is in
/// front of an open microphone -- it is picked up, written down as
/// something you said, and read back to the room as yours -- and
/// echo cancellation is deliberately off here (see
/// `RoomAudio.startMic`), so nothing else is keeping it out.
///
/// Full duplex is the default because it is the one that cannot
/// surprise anybody: a room that goes silent when you speak is a room
/// people report as broken, and headsets are the common case.
///
/// Changed mid-hold, it takes effect on the hold you are in -- the
/// setting is a page away from the key, but a page you can reach with
/// the other hand while the room is running.
export function setTalkHush(on) {
  room.pttHush = on;
  writePref('vox_room.ptt_hush', on);
  hushRoom(on && room.pushToTalk && room.talking);
}

// After letting go, the mic stays open a moment so the last word gets
// through.
const RELEASE_MS = 300;
let releaseTimer = null;

/// The room's mixer has your own strip turned off, so nothing you say
/// here reaches the room or is written down -- whatever your own mute
/// button says. A fader everybody shares, and this room's alone, so it
/// is not yours to clear from the mic and it does not follow you into
/// the next room.
export function mutedByRoom() {
  return room.strips?.[room.you]?.mute === true;
}

/// The hold that has not opened anything yet, or null.
///
/// With the chirp on, a hold arms for [ARM_MS] before the microphone
/// goes live and the chirp fills that time -- see `lib/chirp.js` for
/// why it is before and not after. Let go inside it and nothing was
/// ever said: no mic, no entry, and the chirp cut where it stands.
let arming = null;

/// Whether the press that just arrived has already done its waiting.
///
/// The Android app lends a volume key to the microphone, and it cannot
/// know whether a press is a tap or a hold without waiting a quarter of
/// a second to find out. It fills that wait with its own copy of the
/// chirp -- it has to, since nothing is said to this page until the
/// wait is over -- and only then presses the key here. Arming again
/// would make a phone wait twice for one hold. The app leaves a
/// timestamp to say it has already waited; anything older than one
/// arming is not about this press.
///
/// Read, never called: the app writes a value and this reads it, which
/// is the same direction everything else between the two goes. See
/// ROOM_ANDROID.md.
function armedAlready() {
  try {
    const at = Number(localStorage.getItem('vox_room.ptt_armed') || 0);
    return at > 0 && Date.now() - at < ARM_MS * 2;
  } catch {
    return false;
  }
}

export function talk(on) {
  // Muted on the mixer, holding the key does nothing and must look
  // like nothing: a talk button that lights up while the room is
  // throwing the audio away is the room lying about being listened to.
  if (!room.pushToTalk || !room.mic || mutedByRoom()) return;
  clearTimeout(releaseTimer);
  if (on) {
    // Already holding, or already arming. A key repeats and a button
    // can be pressed again; one hold is one thing said.
    if (room.talking || arming !== null) return;
    if (room.pttChirp !== 'off' && !armedAlready()) {
      chirp(true);
      arming = setTimeout(() => { arming = null; live(); }, ARM_MS);
      return;
    }
    live();
  } else if (arming !== null) {
    // Let go before it opened: nothing happened, and the sound saying
    // something was about to must stop with it.
    clearTimeout(arming);
    arming = null;
    hush();
  } else if (room.talking) {
    room.talking = false;
    releaseTimer = setTimeout(() => {
      if (room.pushToTalk && !room.talking) {
        setMuted(true);
        // Not on the release: the mic is deliberately left open a
        // moment for the last word, and that moment is still a moment
        // with a microphone in front of the speakers.
        hushRoom(false);
      }
    }, RELEASE_MS);
    if (room.pttChirp !== 'off') chirp(false);
  }
}

/// Open the microphone, which is what the hold was for.
function live() {
    room.talking = true;
    // The room goes quiet before the mic opens, and not the other way
    // round -- on half duplex, which is what somebody on speakers
    // holds this key for: what the room is saying is in front of their
    // microphone, and a mic cannot be made deaf to one sound in the
    // room it is in. On a headset there is nothing to be deaf to, so
    // the hold costs you nothing you were hearing. See `setTalkHush`
    // and `RoomAudio.hushRoom`.
    hushRoom(room.pttHush);
    if (room.muted) setMuted(false);
}

/// Cut the room's sound, or let it back in, and say so on the button
/// being held: a room that goes silent for no stated reason is a room
/// somebody reports as broken.
function hushRoom(on) {
  // The sound first and the sign afterwards, so that a button saying
  // the room is quiet is a room that is quiet: anything that threw on
  // the way down leaves the button telling the truth about it.
  audio?.hushRoom(on);
  room.hushed = on;
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
