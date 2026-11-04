/// Who the keyboard belongs to.
///
/// Two keys in this room are the room's rather than the page's. Space is
/// your microphone's, everywhere, because the thing you reach for
/// without looking has to work from wherever you left the focus -- see
/// `App`. Escape is the way back out of anything that has taken Space
/// off you.
///
/// Both questions are the same question, so it is asked in one place:
/// is the focus in something with a cursor in it?

/// What takes Space off the microphone: a box with a cursor in it, a
/// list the keys are choosing from, a dialog waiting to be answered.
/// While the focus is in one of these, Space is its key.
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
/// deliberately: more than one handler asks this about the same
/// Escape, and the first of them blurs. The target is what had the
/// focus when the key went down, and stays that whatever any of them
/// does about it -- so every handler gets the same answer however they
/// are ordered, which is not a thing the live focus can promise.
export function fieldAt(target) {
  const el = target?.closest?.(FIELD);
  return el && !el.closest('dialog') ? el : null;
}
