// Two AIs in one room, and whether they may answer you:
//   node bots.js RYAN_LINK ALEX_LINK
//
// Mostly no browser: this is room state. Each AI answers to its own
// name, has its own fader and its own Stops, and at a pause they are
// all given the turn and only one of them keeps it -- because two of
// them answering at once is two voices over each other.
//
// And when an AI answers *you* is your own switch -- BOTS in the
// footer, one per person -- which is the thing that used to be one
// Answers control per bot that the whole room shared. Off, the AIs
// stay out of what you say; off and called by name, they still answer,
// which is the nuance in it; off meaning never, not even that. None of
// it touches anybody else in the room.
//
// Wants a room with a second bot in it:
//   vox_room --room Lobby --bot Sage --echo --no-stt --tts none

const { is, report, person, strips, type, until, wait, socket } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);
const token = (link) => link.replace(/.*\/join\//, '');

/// What the room did about the last thing said, once it has finished
/// doing it. The wait first is not slack: a pause has to last
/// `reply_after` before it is a pause at all, so without it the room
/// is still listening and looks like a room that decided nothing.
const settled = async (p) => {
  await wait(2000);
  await until(() => (p.agents ?? []).every((a) => a.state === 'listening' || a.state === 'off'), {
    every: 200,
  });
};

/// What the AIs have said, in order, one entry per thing said. A line
/// is sent again as it grows, so the same entry arrives many times and
/// counting events would be counting words.
const byBots = (p) =>
  [...new Map(p.said.filter((e) => e.who === 'Vox' || e.who === 'Sage').map((e) => [e.id, e])).values()];

/// Whether the AIs may answer each person, off the roster the room
/// sends everybody: an AI that answers one person and not the one
/// beside them is a room where the reason has to be visible.
const switches = (p) =>
  Object.fromEntries((p.roster ?? []).map((q) => [q.name, q.bots]));

/// And what the browser makes of it: the mark against each name in the
/// list of who is here. `null` for somebody the AIs may answer, which
/// is everybody to begin with -- the mark is the exception, since one
/// against every name in the room would be a mark that says nothing.
const marks = (page) =>
  page.evaluate(() =>
    Object.fromEntries(
      [...document.querySelectorAll('.rooms .strip')].map((s) => {
        const who = s.querySelector('.label').textContent.replace(/\s*\(you\)\s*/, '').trim();
        const mark = s.querySelector('.bots');
        return [who, mark ? (mark.classList.contains('all') ? 'never' : 'named') : null];
      }),
    ));

(async () => {
  const ryan = await socket(token(ryanLink));
  await wait(400);

  is('two AIs in the room', ryan.agents.map((a) => a.name), ['Vox', 'Sage']);
  is(
    'a fader each, or moving one would move them both',
    ryan.agents.map((a) => a.strip),
    ['@agent', '@agent:Sage'],
  );
  // Shown every pause and left to judge it: the house room is a room
  // people talk in, and an AI that answered every gap in it would be
  // answering conversations that are not its own. Whether it may
  // answer any one person is that person's switch, below.
  is('both judge the pauses in the house', ryan.agents.map((a) => a.mode), ['auto', 'auto']);

  // And the faders really are separate: turning one down leaves the
  // other where it was. A strip is made the first time it is moved, so
  // the one nobody touched is simply not there.
  ryan.send({ type: 'strip', name: '@agent:Sage', gain: 0.25 });
  await wait(500);
  ryan.send({ type: 'enter_room', id: '@Ryan' });
  await wait(600);
  ryan.send({ type: 'enter_room', id: 'Lobby' });
  await wait(700);
  is('the one that was turned down', ryan.faders['@agent:Sage']?.gain, 0.25);
  is('and the other was left alone', '@agent' in ryan.faders, false);

  // Somebody else in the room, in a browser: the switches below are
  // about one person's being nobody else's, so there has to be
  // somebody else. (A link is spent by coming in, so Alex is the
  // browser and Ryan is the socket -- one each.)
  const alex = await person(alexLink);
  await wait(600);

  // Each answers to its own name, and to nobody else's.
  ryan.send({ type: 'text', text: 'Sage, what do you make of the door?' });
  await settled(ryan);
  is('the one named answered', byBots(ryan).map((e) => e.who), ['Sage']);

  ryan.send({ type: 'text', text: 'Vox, and you?' });
  await settled(ryan);
  is('and so did the other, when it was its turn to be named',
    byBots(ryan).map((e) => e.who), ['Sage', 'Vox']);

  // Everybody starts with the AIs allowed to answer them, and the
  // roster says so for everybody: this is not a thing hidden in one
  // browser.
  is('the AIs may answer everybody to begin with', switches(ryan), { Alex: 'any', Ryan: 'any' });
  is('so nobody is marked in the list of who is here',
    await marks(alex.page), { Alex: null, Ryan: null, Vox: null, Sage: null });

  // Off meaning never: nothing of theirs reaches an AI, its own name
  // included.
  ryan.send({ type: 'bots', how: 'never' });
  await wait(600);
  is('one person turned them off', switches(ryan), { Alex: 'any', Ryan: 'never' });
  // And everybody can see which of them it was. An AI that answers one
  // person and not the one beside them is a room where the reason has
  // to be somewhere, or it reads as a fault -- so the mark is on
  // somebody else's screen too, not only on the screen of whoever
  // threw the switch.
  is('and the room can see who it was', await marks(alex.page),
    { Alex: null, Ryan: 'never', Vox: null, Sage: null });
  const off = byBots(ryan).length;
  ryan.send({ type: 'text', text: 'Vox, are you there?' });
  await settled(ryan);
  is('and named, it answers nothing of theirs',
    byBots(ryan).slice(off).map((e) => e.who), []);

  // One person's switch is one person's: the room is a room, and
  // everybody else is answered exactly as before.
  await type(alex.page, 'Vox, is anyone else there?');
  await settled(ryan);
  is('and the person beside them is answered as before',
    byBots(ryan).slice(off).map((e) => e.who), ['Vox']);
  is('and what the one who turned them off said is still in the conversation',
    ryan.said.some((e) => e.text.includes('are you there')), true);

  // Off meaning only-when-named, which is the nuance and the default:
  // a quiet room is still a room you can ask a question in.
  const named = byBots(ryan).length;
  ryan.send({ type: 'bots', how: 'named' });
  await wait(600);
  is('off, but not to its name', switches(ryan), { Alex: 'any', Ryan: 'named' });
  is('and the mark says which way it is off', await marks(alex.page),
    { Alex: null, Ryan: 'named', Vox: null, Sage: null });
  ryan.send({ type: 'text', text: 'Sage, one more thing.' });
  await settled(ryan);
  is('and calling one by name is answered',
    byBots(ryan).slice(named).map((e) => e.who), ['Sage']);

  // It is theirs rather than a room's: it follows them down the hall,
  // into a room of their own and back.
  ryan.send({ type: 'enter_room', id: '@Ryan' });
  await wait(700);
  is('the switch follows them into their own room', switches(ryan), { Ryan: 'named' });
  ryan.send({ type: 'enter_room', id: 'Lobby' });
  await wait(700);
  is('and back', switches(ryan), { Alex: 'any', Ryan: 'named' });

  // And a switch the room has never heard of is refused, to the one
  // tab that asked and nobody else.
  ryan.send({ type: 'bots', how: 'sometimes' });
  await wait(500);
  is('a switch the room has no position for', ryan.notices.length > 0, true);
  is('and nothing moved', switches(ryan).Ryan, 'named');

  ryan.send({ type: 'bots', how: 'any' });
  await wait(600);
  is('and back on', switches(ryan), { Alex: 'any', Ryan: 'any' });

  // Switched off in this room: not silenced, not taken out of the
  // house -- asked nothing, so nothing is thought and nothing is
  // said. It keeps its place in the list and everything set on it.
  const before = byBots(ryan).length;
  ryan.send({ type: 'bot_off', bot: 'Sage', off: true });
  await wait(600);
  is('one of them is switched off here',
    ryan.agents.map((a) => [a.name, a.off]), [['Vox', false], ['Sage', true]]);
  is('and its strip says so rather than promising a turn',
    ryan.agents.find((a) => a.name === 'Sage').state, 'off');
  ryan.send({ type: 'text', text: 'Sage, are you there?' });
  await settled(ryan);
  is('and being named does not wake it', byBots(ryan).slice(before).map((e) => e.who), []);
  is('while the one beside it answers as before', ryan.agents.find((a) => a.name === 'Vox').off, false);

  // It is the room's, like Stops and unlike the BOTS switch: the room
  // down the hall has its own answer, and this one is still off when
  // you come back to it.
  ryan.send({ type: 'enter_room', id: '@Ryan' });
  await wait(700);
  ryan.send({ type: 'enter_room', id: 'Lobby' });
  await wait(700);
  is('and it is still off when you come back',
    ryan.agents.find((a) => a.name === 'Sage').off, true);

  const back = byBots(ryan).length;
  ryan.send({ type: 'bot_off', bot: 'Sage', off: false });
  await wait(600);
  ryan.send({ type: 'text', text: 'Sage, and now?' });
  await settled(ryan);
  is('switched back on, it answers again',
    byBots(ryan).slice(back).map((e) => e.who), ['Sage']);

  // Muted on the mixer is the room *reading* a voice rather than
  // hearing it: nothing is synthesized, so the reply is not paced to
  // any audio and the whole of it lands as fast as it is thought.
  //
  // `--tts none` is silence of the length the words would have taken,
  // which is what makes the difference measurable. The line below is
  // long on purpose: echoed back it is some six hundred characters,
  // which is forty seconds of silence to play and no time at all to
  // read. So "finished inside five seconds" tells the two apart with
  // a margin nothing here is going to close.
  const mouthful = `Vox, ${'tell me about the long slow business of waiting. '.repeat(12)}`;
  const whole = (n) => {
    const said = byBots(ryan).slice(n)[0];
    return !!said && said.text.trim().endsWith('That is all I heard.');
  };

  await settled(ryan);
  const quiet = byBots(ryan).length;
  ryan.send({ type: 'strip', name: '@agent', mute: true });
  await wait(400);
  ryan.send({ type: 'text', text: mouthful });
  await until(() => whole(quiet), { tries: 50, every: 100 });
  is('a muted voice is read rather than heard, and all of it at once', whole(quiet), true);
  is('and it is never shown as speaking, because it is not',
    ryan.agents.find((a) => a.name === 'Vox').state !== 'speaking', true);
  await settled(ryan);

  // And unmuted, the same words are paced to the voice saying them:
  // the reply arrives a phrase at a time, so after the five seconds
  // that were long enough above there is still most of it to come.
  const aloud = byBots(ryan).length;
  ryan.send({ type: 'strip', name: '@agent', mute: false });
  await wait(400);
  ryan.send({ type: 'text', text: mouthful });
  await wait(5000);
  is('heard rather than read, it arrives at the speed it is spoken', whole(aloud), false);
  ryan.send({ type: 'stop' });
  await settled(ryan);

  /// Which voice one bot is wearing, and whose idea it was.
  const wearing = (p, bot = 'Vox') => {
    const b = (p.agents ?? []).find((a) => a.name === bot);
    return { voice: b?.voice ?? null, here: !!b?.voice_here };
  };

  // Which voice an AI speaks in is the house's, and a room may
  // disagree. The house settles one in Settings and every room
  // follows it; a room that has been given its own keeps it, which is
  // for a room that wants a different reader without changing the AI
  // down the hall.
  is('to begin with it is nobody in particular', wearing(ryan), { voice: null, here: false });
  ryan.send({ type: 'room_voice', bot: 'Vox', here: true, voice: 'Alan Watts' });
  await wait(600);
  is('this room gave it one', wearing(ryan), { voice: 'Alan Watts', here: true });

  // A room nobody has had an opinion about is untouched by that: this
  // is one room disagreeing, not the house being settled.
  ryan.send({ type: 'enter_room', id: '@Ryan' });
  await wait(700);
  is('and the room down the hall is where it was', wearing(ryan), { voice: null, here: false });

  // The house settling one reaches every room that has no answer of
  // its own -- and is overruled by the one that has.
  ryan.send({ type: 'voice', bot: 'Vox', voice: 'Deanna' });
  await wait(700);
  is('the house settled a voice, and this room took it', wearing(ryan), { voice: 'Deanna', here: false });
  ryan.send({ type: 'enter_room', id: 'Lobby' });
  await wait(700);
  is('and the room with its own answer kept it', wearing(ryan), { voice: 'Alan Watts', here: true });

  // Handing the question back: the room stops having an opinion and
  // hears the house again. Not the same as choosing the AI's own
  // voice here, which is an opinion like any other.
  ryan.send({ type: 'room_voice', bot: 'Vox', here: true, voice: null });
  await wait(600);
  is('its own voice here is still an answer', wearing(ryan), { voice: null, here: true });
  ryan.send({ type: 'room_voice', bot: 'Vox', here: false });
  await wait(600);
  is('and handing the question back takes the house s again', wearing(ryan), { voice: 'Deanna', here: false });
  ryan.send({ type: 'voice', bot: 'Vox', voice: null });
  await wait(600);

  // Whether a voice talking over one cuts it off is that bot's own,
  // and the one thing about an AI that is still set per AI.
  is('both cut off by a voice to begin with',
    ryan.agents.map((a) => a.stops), ['interrupted', 'interrupted']);
  ryan.send({ type: 'stops', bot: 'Vox', stops: 'finished' });
  await wait(500);
  is('one of them left to finish what it was saying',
    ryan.agents.map((a) => a.stops), ['finished', 'interrupted']);

  // That one is per room, and a tab coming into a room is told it:
  // stepping out and back in asks the room all over again.
  await settled(ryan);
  ryan.send({ type: 'enter_room', id: '@Ryan' });
  await wait(700);
  // A room of your own has the room's own AI in it and nobody else's:
  // a second one is in a room because the owner put it there, and
  // nobody has put this one in anybody's own room.
  is('a room of your own has the first AI', ryan.agents.map((a) => a.name), ['Vox']);
  is('answering every pause, which is what being alone with it is for',
    ryan.agents.map((a) => a.mode), ['always']);
  is('and cut off by a voice, until this room is told otherwise',
    ryan.agents.map((a) => a.stops), ['interrupted']);
  ryan.send({ type: 'enter_room', id: 'Lobby' });
  await wait(700);
  is('and the house remembered what stops each of them there',
    ryan.agents.map((a) => a.stops), ['finished', 'interrupted']);

  // And on screen: a strip each under the room, each saying what that
  // AI is doing -- with no fader on it, because nobody has ever wanted
  // one of these louder or quieter than the rest of the room, and two
  // things to set: when talking over it cuts it off, and which voice
  // it speaks in here.
  is('the people, then a strip for each AI', await strips(alex.page), [
    'Alex (you)',
    'Ryan',
    'Vox',
    'Sage',
  ]);
  is(
    'and each has its own Stops and its own voice, and nothing else to set',
    await alex.page.evaluate(() =>
      [...document.querySelectorAll('.rooms .strip select')].map((s) => [
        s.getAttribute('aria-label'),
        s.value,
      ]),
    ),
    [
      ['When Vox stops speaking', 'finished'],
      ['The voice Vox speaks in here', 'house'],
      ['When Sage stops speaking', 'interrupted'],
      ['The voice Sage speaks in here', 'house'],
    ],
  );
  // A voice the room made is as loud as the room asked it to be.
  is(
    'and no fader on an AI, where every person has one',
    await alex.page.evaluate(() =>
      [...document.querySelectorAll('.rooms .strip')].map((s) => [
        s.querySelector('.label').textContent.replace(/\s+/g, ' ').trim(),
        !!s.querySelector('input[type=range]'),
      ]),
    ),
    [['Alex (you)', true], ['Ryan', true], ['Vox', false], ['Sage', false]],
  );
  is(
    'and says what each of them is doing, separately from each other and from the people',
    await alex.page.evaluate(() =>
      Object.fromEntries(
        [...document.querySelectorAll('.rooms .strip')].map((e) => [
          // The label has a hard space in it, as the room renders it.
          e.querySelector('.label').textContent.replace(/\s+/g, ' ').trim(),
          e.querySelector('.state').textContent.replace(/\s+/g, ' ').trim(),
        ]),
      ),
    ),
    { 'Alex (you)': 'mic on', Ryan: 'no mic', Vox: 'listening', Sage: 'listening' },
  );

  // Setting that one from the browser is that one bot's setting.
  await alex.page.selectOption(
    '.rooms .strip select[aria-label="When Sage stops speaking"]',
    'finished',
  );
  await wait(800);
  is('changed from the browser', ryan.agents.map((a) => a.stops), ['finished', 'finished']);

  // The switch that is not a bot's is in the footer, where it is
  // reached mid-conversation, and it is the browser's own person it
  // speaks for.
  await alex.page.click('footer .bots-switch');
  await wait(800);
  is('and the one in the footer is each person’s own',
    switches(ryan), { Alex: 'named', Ryan: 'any' });

  await alex.browser.close();
  report();
})();
