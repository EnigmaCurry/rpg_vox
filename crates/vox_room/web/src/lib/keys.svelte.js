/// Who the keyboard belongs to.
///
/// Two keys in this room are the room's rather than the page's. One is
/// the talk key -- Space out of the box, and whatever you have bound it
/// to after that -- which belongs to your microphone everywhere, because
/// the thing you reach for without looking has to work from wherever you
/// left the focus. The other is Escape, which is the way back out of
/// anything that has taken the first one off you.
///
/// Both questions are the same question, so it is asked in one place: is
/// the focus in something with a cursor in it?

/// Whether the room is being used with a finger and nothing else.
///
/// The talk key is the room's best idea on a keyboard and a thing that
/// does not exist on a phone, and the room says so out loud in more
/// than one place: the door explains push to talk by naming the key you
/// hold. Naming a key at somebody who has none is worse than saying
/// nothing, so the few sentences that do it ask here first.
///
/// Touch-primary rather than narrow, because the question is whether
/// there are keys and not how wide the window is: a small window on a
/// laptop still has them, and a tablet with one attached has them too.
/// It is live, so a keyboard appearing is noticed -- the same media
/// query hides the key hints in `app.css`, and the two must not be able
/// to disagree.
const COARSE = '(hover: none) and (pointer: coarse)';
export const byTouch = $state({ only: false });
if (typeof window !== 'undefined' && window.matchMedia) {
  const mq = window.matchMedia(COARSE);
  byTouch.only = mq.matches;
  mq.addEventListener('change', (e) => (byTouch.only = e.matches));
}

// -- what takes a key off the room --------------------------------------

/// What takes the talk key off the microphone: a box with a cursor in
/// it, a list the keys are choosing from, a dialog waiting to be
/// answered. While the focus is in one of these, the key is its.
export const TYPING =
  'input, textarea, select, [contenteditable=""], [contenteditable="true"], dialog';

/// Only the ones you can be let out of.
///
/// A dialog is not among them. Escape there closes the dialog, which is
/// the browser's own doing and is what somebody pressing it means --
/// blurring a field inside one would leave the dialog open with the
/// cursor nowhere, which is a worse place to be than where they were.
const FIELD = 'input, textarea, select, [contenteditable=""], [contenteditable="true"]';

/// The box a key was pressed in, if it was pressed in one.
///
/// Asked of the event's target rather than of `document.activeElement`,
/// deliberately: more than one handler asks this about the same Escape,
/// and the first of them blurs. The target is what had the focus when
/// the key went down, and stays that whatever any of them does about it
/// -- so every handler gets the same answer however they are ordered,
/// which is not a thing the live focus can promise.
export function fieldAt(target) {
  const el = target?.closest?.(FIELD);
  return el && !el.closest('dialog') ? el : null;
}

// -- which key talks -----------------------------------------------------

/// Space, which is what it was before it was a choice.
export const DEFAULT = { code: 'Space', ctrl: false, alt: false, shift: false, meta: false };

/// Where the choice is kept. This browser's, like the theme and for the
/// same reason: it is a fact about a keyboard, not about a person, and
/// the same person at a desk and on a laptop may want different answers.
/// Nothing about it goes to the server.
const KEY = 'vox_room.talk_key';

function stored() {
  try {
    const raw = JSON.parse(localStorage.getItem(KEY) ?? 'null');
    // A stored binding is only as good as the room it came from. One
    // written by an older version, or edited by hand, must not leave
    // somebody with a key that cannot talk and no way to see why.
    if (!raw || typeof raw.code !== 'string' || !raw.code) return DEFAULT;
    return {
      code: raw.code,
      ctrl: !!raw.ctrl,
      alt: !!raw.alt,
      shift: !!raw.shift,
      meta: !!raw.meta,
    };
  } catch {
    return DEFAULT;
  }
}

/// The key your microphone is on. Read `talk.bind` to draw it; `bindTalk`
/// changes it.
export const talk = $state({ bind: stored() });

export function bindTalk(bind) {
  talk.bind = { ...DEFAULT, ...bind };
  try {
    localStorage.setItem(KEY, JSON.stringify(talk.bind));
  } catch {}
}

/// Whether this is still the key the room came with.
export const isDefault = (bind) =>
  bind.code === DEFAULT.code && !bind.ctrl && !bind.alt && !bind.shift && !bind.meta;

/// Going down: the key *and* the modifiers, so a binding with Ctrl in it
/// is not fired by the bare key.
export function pressed(e, bind) {
  return (
    e.code === bind.code &&
    e.ctrlKey === bind.ctrl &&
    e.altKey === bind.alt &&
    e.shiftKey === bind.shift &&
    e.metaKey === bind.meta
  );
}

/// Coming up: the key alone, and deliberately not the modifiers.
///
/// This is the whole reason push to talk can be bound to a combination
/// at all. Hold Ctrl+Space, let go of Ctrl first, and the keyup for
/// Space arrives with `ctrlKey` already false -- so a release that
/// insisted on the modifiers would never match, and the microphone
/// would stay open with nothing on the screen saying so. In a room where
/// everybody hears everybody that is the worst thing this app can do, so
/// release is as easy to match as it can be made. (The other half of
/// that promise is in `App`: losing the window closes the mic too.)
export const released = (e, bind) => e.code === bind.code;

// -- what a key is called ------------------------------------------------

const NAMED = {
  Space: 'Space',
  Backspace: 'Backspace',
  Delete: 'Del',
  Insert: 'Ins',
  Home: 'Home',
  End: 'End',
  PageUp: 'PgUp',
  PageDown: 'PgDn',
  Backquote: '`',
  Minus: '-',
  Equal: '=',
  BracketLeft: '[',
  BracketRight: ']',
  Backslash: '\\',
  Semicolon: ';',
  Quote: "'",
  Comma: ',',
  Period: '.',
  Slash: '/',
  CapsLock: 'Caps',
};

const MAC = () => /mac|iphone|ipad/i.test(navigator.platform || navigator.userAgent || '');

function one(code) {
  if (code.startsWith('Key')) return code.slice(3);
  if (code.startsWith('Digit')) return code.slice(5);
  if (code.startsWith('Arrow')) return code.slice(5);
  if (code.startsWith('Numpad')) return `Num ${code.slice(6) || 'pad'}`;
  return NAMED[code] ?? code;
}

/// What to print on the button. Short, because it sits in a `<kbd>` on
/// the talk button and the button has a width to keep.
export function keyName(bind) {
  const bits = [];
  if (bind.ctrl) bits.push('Ctrl');
  if (bind.alt) bits.push(MAC() ? 'Opt' : 'Alt');
  if (bind.shift) bits.push('Shift');
  if (bind.meta) bits.push(MAC() ? 'Cmd' : 'Meta');
  bits.push(one(bind.code));
  return bits.join('+');
}

// -- which keys cannot be it ---------------------------------------------

const MODIFIER = new Set([
  'ShiftLeft', 'ShiftRight', 'ControlLeft', 'ControlRight',
  'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight',
]);

/// Keys the room has already spent, and what on.
const SPENT = {
  Escape: 'Escape is how you get the keyboard back out of a box you are typing in.',
  Enter: 'Enter works the button you are on, and sends what you have typed.',
  NumpadEnter: 'Enter works the button you are on, and sends what you have typed.',
  Tab: 'Tab is how a keyboard gets around the room at all.',
  F11: 'The browser keeps F11 for full screen.',
  F12: 'The browser keeps F12 for its own tools.',
};

/// Combinations the browser never really hands over, with what they
/// would do instead. The room cannot refuse these on your behalf once
/// they are bound -- it is never asked -- so they are refused here.
const TAKEN = {
  KeyW: 'that closes the tab, and the audio with it',
  KeyT: 'that opens a new tab',
  KeyN: 'that opens a new window',
  KeyQ: 'that quits the browser',
};

/// Why this press cannot be the talk key, or `null` if it can.
///
/// Three kinds of no, and nothing else -- a key the room is simply not
/// given never reaches this at all, so there is no need to guess at a
/// list of those: press one and the room goes on waiting for a key,
/// which is the truth about it.
export function refuse(e) {
  if (MODIFIER.has(e.code)) {
    return 'Shift, Ctrl, Alt and Cmd are held down while you use them with other keys, so one on its own cannot be the talk key. Hold it and press something.';
  }
  if (SPENT[e.code]) return SPENT[e.code];
  if ((e.ctrlKey || e.metaKey) && TAKEN[e.code]) {
    return `The browser keeps that one for itself — ${TAKEN[e.code]}.`;
  }
  return null;
}

/// The binding a press means.
export const bindOf = (e) => ({
  code: e.code,
  ctrl: e.ctrlKey,
  alt: e.altKey,
  shift: e.shiftKey,
  meta: e.metaKey,
});
