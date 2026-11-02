// Two AIs in one room:
//   node bots.js RYAN_LINK
//
// No browser: this is room state. Each AI answers to its own name, has
// its own fader and its own say in when it speaks, and at a pause they
// are all given the turn and only one of them keeps it -- because two
// of them answering at once is two voices over each other.
//
// Wants a room with a second bot in it:
//   vox_room --room Lobby --bot Sage --echo --no-stt --tts none

const { is, report, person, strips, until, wait, socket } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);
const ryanToken = ryanLink.replace(/.*\/join\//, '');

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

/// Back to waiting to be named: a bot that has just been named answers
/// the next pause too, and one bot answering keeps the room from being
/// quiet for all of them, so this takes as long as it takes.
const notEngaged = (p) =>
  until(() => (p.agents ?? []).every((a) => a.mode !== 'auto'), { every: 200 });

/// What the AIs have said, in order, one entry per thing said. A line
/// is sent again as it grows, so the same entry arrives many times and
/// counting events would be counting words.
const byBots = (p) =>
  [...new Map(p.said.filter((e) => e.who === 'Vox' || e.who === 'Sage').map((e) => [e.id, e])).values()];

(async () => {
  const ryan = await socket(ryanToken);
  await wait(400);

  is('two AIs in the room', ryan.agents.map((a) => a.name), ['Vox', 'Sage']);
  is(
    'a fader each, or moving one would move them both',
    ryan.agents.map((a) => a.strip),
    ['@agent', '@agent:Sage'],
  );
  is('both wait to be named in the house', ryan.agents.map((a) => a.mode), ['named', 'named']);

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

  // Each answers to its own name, and to nobody else's.
  ryan.send({ type: 'text', text: 'Sage, what do you make of the door?' });
  await settled(ryan);
  is('the one named answered', byBots(ryan).map((e) => e.who), ['Sage']);

  is('and being named starts a back-and-forth with that one',
    ryan.agents.map((a) => `${a.name}:${a.mode}`), ['Vox:named', 'Sage:auto']);
  await notEngaged(ryan);

  ryan.send({ type: 'text', text: 'Vox, and you?' });
  await settled(ryan);
  is('and so did the other, when it was its turn to be named',
    byBots(ryan).map((e) => e.who), ['Sage', 'Vox']);
  await notEngaged(ryan);

  // When one bot in the room answers is that bot's business.
  ryan.send({ type: 'mode', bot: 'Sage', mode: 'always' });
  await wait(500);
  is('one of them set to answer every pause', ryan.agents.map((a) => a.mode), ['named', 'always']);

  // A pause, and only Sage has anything to do with it.
  const before = byBots(ryan).length;
  ryan.send({ type: 'text', text: 'anyway' });
  await settled(ryan);
  const answered = byBots(ryan).slice(before);
  is('the one that answers every pause took it', answered.map((e) => e.who), ['Sage']);

  // Both on every pause: both are given the turn, and the first to say
  // something keeps it. Which of them wins is a race -- that only one
  // of them says anything is the point.
  await notEngaged(ryan);
  ryan.send({ type: 'mode', bot: 'Vox', mode: 'always' });
  await wait(500);
  is('both answer every pause now', ryan.agents.map((a) => a.mode), ['always', 'always']);
  const two = byBots(ryan).length;
  ryan.send({ type: 'text', text: 'so where does that leave us' });
  await settled(ryan);
  const pause = byBots(ryan).slice(two);
  is('one of them answered the pause', pause.length, 1);
  is('and it was one of the two', ['Vox', 'Sage'].includes(pause[0]?.who), true);

  // Silenced, a bot is out of the conversation without being out of
  // the room: it answers nothing, not even its own name.
  await settled(ryan);
  ryan.send({ type: 'mode', bot: 'Sage', mode: 'never' });
  await wait(500);
  is('one of them silenced', ryan.agents.map((a) => a.mode), ['always', 'never']);
  is('and its strip says so rather than "listening"',
    ryan.agents.map((a) => a.state), ['listening', 'off']);
  is('and still in the room, on its own fader',
    ryan.agents.map((a) => a.strip), ['@agent', '@agent:Sage']);
  const silent = byBots(ryan).length;
  ryan.send({ type: 'text', text: 'Sage, are you still there?' });
  await settled(ryan);
  is('named, and the other one took the pause instead',
    byBots(ryan).slice(silent).map((e) => e.who), ['Vox']);
  ryan.send({ type: 'mode', bot: 'Sage', mode: 'always' });
  await wait(500);
  is('and it comes back on', ryan.agents.map((a) => a.mode), ['always', 'always']);

  // Whether a voice talking over one cuts it off is that bot's as
  // well, and separate from when it answers.
  is('both cut off by a voice to begin with',
    ryan.agents.map((a) => a.stops), ['interrupted', 'interrupted']);
  ryan.send({ type: 'stops', bot: 'Vox', stops: 'finished' });
  await wait(500);
  is('one of them left to finish what it was saying',
    ryan.agents.map((a) => a.stops), ['finished', 'interrupted']);

  // What each was set to is each bot's, and a tab coming into the room
  // is told so: stepping out and back in asks the room all over again.
  await settled(ryan);
  ryan.send({ type: 'mode', bot: 'Sage', mode: 'named' });
  await wait(500);
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
  is('and the house remembered what each of them was set to there',
    ryan.agents.map((a) => a.mode), ['always', 'named']);
  is('and what stops each of them there',
    ryan.agents.map((a) => a.stops), ['finished', 'interrupted']);

  // And on screen: a fader each, under the room, each saying what that
  // AI is doing and when it answers. One control for two bots is the
  // mistake this is here to catch.
  const alex = await person(alexLink);
  is('the people, then a strip for each AI', await strips(alex.page), [
    'Alex (you)',
    'Ryan',
    'Vox',
    'Sage',
  ]);
  is(
    'and each has its own Answers, and its own Stops under it',
    await alex.page.evaluate(() =>
      [...document.querySelectorAll('.rooms .strip select')].map((s) => [
        s.getAttribute('aria-label'),
        s.value,
      ]),
    ),
    [
      ['When Vox speaks', 'always'],
      ['When Vox stops speaking', 'finished'],
      ['When Sage speaks', 'named'],
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

  // Setting one from the browser is that one's setting.
  await alex.page.selectOption('.rooms .strip select[aria-label="When Sage speaks"]', 'always');
  await wait(800);
  is('changed from the browser', ryan.agents.map((a) => a.mode), ['always', 'always']);
  await alex.page.selectOption(
    '.rooms .strip select[aria-label="When Sage stops speaking"]',
    'finished',
  );
  await wait(800);
  is('and so is what stops it', ryan.agents.map((a) => a.stops), ['finished', 'finished']);

  await alex.browser.close();
  report();
})();
