// A room off the record, and a lobby that is never on it:
//   node record.js RYAN_LINK ALEX_LINK MO_LINK
//
// Two sockets and one browser. The sockets are for what the room does
// -- the AI leaving, the conversation taking nothing, the note at each
// end -- and the browser is for the control that does it, which is a
// dropdown over the faders with a question in front of it.
//
// Nothing is transcribed here in any case (`--no-stt`: there are no
// models), so what stands in for somebody talking is somebody typing.
// The room takes them as the same act done two ways, and refuses both
// off the record.
//
// Wants the usual rooms, with the hall in front of them:
//   vox_room --room Hall --room Lobby --room Studio --echo --no-stt --tts none

const { is, report, wait, until, person, socket, strips, goTo } = require('./room.js');
const [ryanLink, alexLink, moLink] = process.argv.slice(2);
const token = (l) => l.replace(/.*\/join\//, '');

/// What a socket has been sent, one entry per entry: a reply arrives
/// again and again as it is spoken, so the same id is in `said` several
/// times over and counting the list would count the speaking.
const entries = (p, is) => {
  const seen = new Map();
  for (const e of p.said) if (is(e)) seen.set(e.id, e.text);
  return [...seen.values()];
};

/// The notes in the conversation: the one line in it nobody said.
const notes = (p) => entries(p, (e) => e.kind === 'note');

/// What somebody actually said, which is what must not grow while the
/// room is off the record.
const words = (p) => entries(p, (e) => e.kind !== 'note');

(async () => {
  const ryan = await socket(token(ryanLink));
  const alex = await socket(token(alexLink));
  await wait(400);

  is('a room is written down to begin with', ryan.transcribing, true);
  is('and that is a thing it can be asked about', ryan.mayTranscribe, true);
  is('with the AI in it', ryan.agents.map((a) => a.name), ['Vox']);

  // Something said while the room is on the record, so there is a line
  // in it that has to still be there afterwards. Waited out to the end
  // of the AI's turn: a reply that is still being spoken when the room
  // goes off the record stops where it is, and with no voice here (the
  // room is started `--tts none`) not a word of it was ever heard, so
  // the room keeps none of it -- which is right, and is not what this
  // is about.
  ryan.send({ type: 'text', text: 'Vox, before anything' });
  await until(() => ryan.said.some((e) => e.who === 'Vox'));
  await wait(2500);
  // Each against its own: two sockets are two connections, and what
  // must not grow is what each of them has been sent.
  const before = { ryan: words(ryan).length, alex: words(alex).length };
  is('both were sent what was said', before, { ryan: 2, alex: 2 });

  // Off the record: one person asks, and it is the room that changes.
  ryan.send({ type: 'transcribe', on: false });
  await until(() => ryan.transcribing === false);
  await wait(600);
  is('she is off the record too, without asking', alex.transcribing, false);
  is('and the AI is out of the room', alex.agents, []);
  is('one note, saying who did it', notes(ryan).length, 1);
  is('and what it means for everybody',
    /^Ryan turned transcription off\. Nothing said in this room from here is written down, and Vox is out of the room\.$/
      .test(notes(ryan)[0] ?? ''), true);
  is('said to the room, not to the one who did it', notes(alex).length, 1);

  // Nothing goes into it while it is off -- not hers, and no answer.
  alex.send({ type: 'text', text: 'is anybody writing this down' });
  await wait(2000);
  is('what is typed is refused', words(alex).length, before.alex);
  is('and the one who typed it is told why',
    /off the record/.test(alex.notices.at(-1) ?? ''), true);
  is('and nobody is answered', words(ryan).length, before.ryan);

  // Back on, by the other one: the AI returns and the second note says
  // who asked for that.
  alex.send({ type: 'transcribe', on: true });
  await until(() => alex.transcribing === true);
  await wait(600);
  is('the AI is back in the room', ryan.agents.map((a) => a.name), ['Vox']);
  is('a note at the other end as well', notes(ryan).length, 2);
  is('saying who turned it back on',
    /^Alex turned transcription back on\./.test(notes(ryan)[1] ?? ''), true);
  ryan.send({ type: 'text', text: 'Vox, and after' });
  await until(() => words(ryan).some((t) => /and after/.test(t)));
  is('and what was said before it is still there',
    words(ryan).some((t) => /before anything/.test(t)), true);

  // The lobby is the one room with no answer to give. Ryan walks back
  // into the hall he landed in.
  ryan.send({ type: 'enter_room', id: 'Hall' });
  await until(() => ryan.where?.id === 'Hall');
  await wait(400);
  is('the lobby is never written down', ryan.transcribing, false);
  is('and has nothing to turn on', ryan.mayTranscribe, false);
  is('nor an AI in it', ryan.agents, []);
  ryan.send({ type: 'transcribe', on: true });
  await wait(600);
  is('asking anyway is refused', ryan.transcribing, false);
  is('and told so', /lobby/.test(ryan.notices.at(-1) ?? ''), true);
  // His socket is carrying the two notes the Lobby wrote -- a socket
  // keeps what it has been sent, whichever room it was in -- so what
  // the hall must not have written is a third.
  is('and nothing is written down about the asking', notes(ryan).length, 2);

  // -- the control itself ----------------------------------------------

  // A third person, in a browser: the dropdown over the faders. An
  // invite link is spent by Join Audio, so this is somebody else's --
  // Ryan's and Alex's are already in the room on sockets.
  const her = await person(moLink);
  const picked = () => her.page.inputValue('.rooms .is select');
  is('the room says it is written down', await picked(), 'on');
  is('and the AI is on a fader', (await strips(her.page)).includes('Vox'), true);
  is('with a box to type into', await her.page.evaluate(() => !!document.querySelector('.say textarea')), true);

  // Asked first, because it changes the room for everyone in it.
  await her.page.selectOption('.rooms .is select', 'off');
  await her.page.waitForSelector('dialog.ask[open]');
  is('the question names what goes',
    /off the record/.test(await her.page.textContent('dialog.ask p')), true);

  // Cancelled, and nothing happened -- including to the dropdown,
  // which must not be left showing an answer the room never gave.
  await her.page.click('dialog.ask .ask-actions button:not(.primary)');
  await her.page.waitForTimeout(400);
  is('cancelling leaves the room written down', await picked(), 'on');
  is('and the AI where it was', (await strips(her.page)).includes('Vox'), true);

  // And again, through with it.
  await her.page.selectOption('.rooms .is select', 'off');
  await her.page.waitForSelector('dialog.ask[open]');
  await her.page.click('dialog.ask .ask-actions button.primary');
  await her.page.waitForTimeout(900);
  is('the room is off the record', await picked(), 'off');
  is('the AI is off the faders', (await strips(her.page)).includes('Vox'), false);
  is('the box you type into is gone', await her.page.evaluate(() => !!document.querySelector('.say textarea')), false);
  is('and the column says what the room is now',
    /Live audio only/.test(await her.page.textContent('.rooms .is')), true);
  is('the other socket in the room was told', alex.transcribing, false);
  // What a room off the record looks like, to be looked at: no AI on
  // the faders, no box in the footer, and the note in the
  // conversation where the words would have been.
  await her.page.screenshot({ path: '/tmp/vox-room-off-the-record.png' });

  // The hall, in the browser: nothing to name, nothing to start, and
  // nothing to read back.
  await goTo(her.page, 'Hall');
  is('the lobby offers no switch', await her.page.evaluate(() => !!document.querySelector('.rooms .is select[disabled]')), true);
  is('no New conversation', await her.page.evaluate(() => !!document.querySelector('.bar .fresh')), false);
  is('no History', await her.page.evaluate(() => !!document.querySelector('.bar .picker')), false);
  is('and no name for a conversation it never has',
    await her.page.evaluate(() => !!document.querySelector('.bar input.name')), false);
  is('it says why instead',
    /written down/.test(await her.page.textContent('.bar')), true);
  await her.page.screenshot({ path: '/tmp/vox-room-lobby.png' });

  await her.browser.close();
  report();
})();
