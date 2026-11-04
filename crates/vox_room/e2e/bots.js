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

  // And on screen: a fader each, under the room, each saying what that
  // AI is doing -- and one Stops to set, which is all there is to set
  // about an AI now.
  is('the people, then a strip for each AI', await strips(alex.page), [
    'Alex (you)',
    'Ryan',
    'Vox',
    'Sage',
  ]);
  is(
    'and each has its own Stops, and nothing else to set',
    await alex.page.evaluate(() =>
      [...document.querySelectorAll('.rooms .strip select')].map((s) => [
        s.getAttribute('aria-label'),
        s.value,
      ]),
    ),
    [
      ['When Vox stops speaking', 'finished'],
      ['When Sage stops speaking', 'interrupted'],
    ],
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
