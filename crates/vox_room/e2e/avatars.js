// The square beside everything somebody says:
//   node avatars.js RYAN_LINK
//
// Every line in the conversation carries a face: the picture when one
// has been made, and otherwise the tint that name already has with its
// initials in it. The column has to line up either way, which is the
// thing worth checking on a real screen -- a placeholder of a
// different size would unalign every name under it.
//
// run.sh puts the pictures and the lines there, since making one is a
// thing only a model with a picture server does.

const { is, report, person, wait } = require('./room.js');
const [ryanLink] = process.argv.slice(2);

(async () => {
  const ryan = await person(ryanLink);
  await ryan.page.waitForSelector('.entry .face');
  // The pictures are fetched over HTTP, so give them a moment to land
  // before asking which branch each square took.
  await wait(500);

  const faces = await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.said > .entry')].map((e) => {
      const face = e.querySelector(':scope > .face');
      const img = face?.querySelector('img');
      const box = face?.getBoundingClientRect();
      return {
        who: e.querySelector('.name')?.textContent.trim() ?? '',
        // Which branch drew it: a picture, or their initials.
        src: img?.getAttribute('src') ?? null,
        // Whether the browser actually decoded it, rather than firing
        // onerror and falling back. 0 means it never loaded.
        drawn: img ? img.naturalWidth : 0,
        initials: face?.querySelector('.initials')?.textContent.trim() ?? null,
        w: box ? Math.round(box.width) : 0,
        h: box ? Math.round(box.height) : 0,
      };
    }),
  );

  is('every line has a square', faces.length >= 3 && faces.every((f) => f.w > 0), true);
  is(
    'and every square is the same square',
    new Set(faces.map((f) => `${f.w}x${f.h}`)).size,
    1,
  );
  is('which is square', faces[0].w, faces[0].h);

  await ryan.page.screenshot({ path: '/tmp/vox-room-avatars.png' });

  const ryansOwn = faces.find((f) => f.who === 'Ryan');
  const voxs = faces.find((f) => f.who === 'Vox');
  const alexs = faces.find((f) => f.who === 'Alex');

  // A face that was made: the picture, at the version the room sent,
  // and actually decoded by the browser.
  is('Ryan has a picture', /^\/api\/avatar\/Ryan\?v=\d+$/.test(ryansOwn.src ?? ''), true);
  is('and the browser drew it', ryansOwn.drawn, 8);
  is('so no initials in its place', ryansOwn.initials, null);

  // The AI has one too, under its own name.
  is('Vox has a picture', /^\/api\/avatar\/Vox\?v=\d+$/.test(voxs.src ?? ''), true);
  is('and the browser drew that', voxs.drawn, 8);

  // Pixel art, blown up from 8 pixels into a 36-pixel square, is
  // drawn pixel by pixel: smoothing it is what the square is not for.
  // (A face bigger than its square -- a photograph somebody dropped
  // into the room -- is smoothed instead; see the `drop` suite.)
  is(
    'and a face smaller than its square is drawn pixel by pixel',
    await ryan.page.evaluate(() => {
      const img = document.querySelector('.said > .entry > .face img');
      return img ? getComputedStyle(img).imageRendering : null;
    }),
    'pixelated',
  );

  // And nobody who has none is left with a hole: the tint of their
  // name, with enough of it to tell two people apart.
  is('Alex has no picture', alexs.src, null);
  is('and shows initials instead', alexs.initials, 'A');

  // Asked for once per version, however many lines that person said:
  // the address carries the version and the answer is immutable.
  const fetched = await ryan.page.evaluate(() =>
    performance
      .getEntriesByType('resource')
      .filter((r) => r.name.includes('/api/avatar/'))
      .map((r) => r.name.replace(/^https?:\/\/[^/]+/, '')),
  );
  is(
    "Ryan's picture was fetched once",
    fetched.filter((u) => u.startsWith('/api/avatar/Ryan')).length,
    1,
  );

  // A picture the AI made, in the conversation where the room can
  // look at it and say what to change.
  const cards = await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.entry.picture-entry')].map((e) => {
      const img = e.querySelector('.shot img');
      return {
        label: e.querySelector('.time')?.textContent.trim() ?? '',
        asked: e.querySelector('.asked')?.textContent.trim() ?? '',
        src: img?.getAttribute('src') ?? null,
        drawn: img ? img.naturalWidth : 0,
        spinning: !!e.querySelector('.shot .spinner'),
        broke: e.querySelector('.shot .broke')?.textContent.trim() ?? null,
        why: e.querySelector('.hint')?.textContent.trim() ?? null,
      };
    }),
  );
  is('both pictures are in the conversation', cards.length, 2);
  // Numbered, because the number is what somebody says out loud and
  // what set_avatar is given.
  is('the first is numbered', cards[0].label.endsWith('picture 1'), true);
  is('and shows what it was asked for', cards[0].asked, 'a tired wizard');
  is('and the browser drew it', cards[0].drawn, 8);
  is('with no spinner on it', cards[0].spinning, false);

  // The second was still being drawn when the room was last stopped,
  // which is the one case a spinner must not survive: the call it was
  // waiting on died with the process, so the card says so rather than
  // promising for ever that something is still happening. (The
  // spinner itself is a live state, checked in the Rust tests where a
  // picture can be begun without finishing it.)
  is('the second is numbered too', cards[1].label.endsWith('picture 2'), true);
  is('and says it never came', cards[1].broke, "didn't come");
  is('rather than spinning for ever', cards[1].spinning, false);
  is('with the reason under it', cards[1].why, 'the room restarted while it was being made');
  is('and no image', cards[1].src, null);
  is('but it still says what it would have been', cards[1].asked, 'the same wizard, rested');

  // Clicking a face opens who they are.
  await ryan.page.evaluate(() => {
    const mine = [...document.querySelectorAll('.said > .entry')].find(
      (e) => e.querySelector('.name')?.textContent.trim() === 'Ryan',
    );
    mine.querySelector(':scope > .face').click();
  });
  await ryan.page.waitForSelector('.who-card');
  await wait(300);
  is(
    'a face opens onto who they are',
    await ryan.page.evaluate(() => {
      const c = document.querySelector('.who-card');
      return {
        who: c.querySelector('h2').textContent.trim(),
        bio: c.querySelector('.bio')?.textContent.trim() ?? null,
        // What the picture was asked for is not shown here: that is
        // the AI's note for making the next one, not a thing to read
        // back at somebody on their own card.
        asked: c.querySelector('.asked')?.textContent.replace(/\s+/g, ' ').trim() ?? null,
        tags: [...c.querySelectorAll('.tag')].map((t) => t.textContent.trim()),
        drawn: c.querySelector('img')?.naturalWidth ?? 0,
        // Blown up from 8 pixels to 160 here, so pixel by pixel, for
        // the same reason as in the conversation.
        rendering: getComputedStyle(c.querySelector('img')).imageRendering,
      };
    }),
    {
      who: 'Ryan',
      bio: 'A ranger from the north, quiet about it.',
      asked: null,
      tags: ['reads English', 'here'],
      drawn: 8,
      rendering: 'pixelated',
    },
  );

  // Escape closes it, and nobody else's screen ever had it.
  await ryan.page.keyboard.press('Escape');
  await wait(200);
  is(
    'and closes again',
    await ryan.page.evaluate(() => !!document.querySelector('.who-card')),
    false,
  );

  // An AI has one too, and the card says it is not a person.
  await ryan.page.evaluate(() => {
    const vox = [...document.querySelectorAll('.said > .entry')].find(
      (e) => e.querySelector('.name')?.textContent.trim() === 'Vox',
    );
    vox.querySelector(':scope > .face').click();
  });
  await ryan.page.waitForSelector('.who-card');
  await wait(300);
  is(
    'an AI is not a person, and says so',
    await ryan.page.evaluate(() => {
      const c = document.querySelector('.who-card');
      return {
        who: c.querySelector('h2').textContent.trim(),
        tags: [...c.querySelectorAll('.tag')].map((t) => t.textContent.trim()),
      };
    }),
    { who: 'Vox', tags: ['an AI', 'speaks English', 'here'] },
  );
  await ryan.page.screenshot({ path: '/tmp/vox-room-profile.png' });
  await ryan.page.keyboard.press('Escape');
  await wait(200);

  // Who is here, with a face each: the list is easier to read by
  // picture than by name, and the same square does it.
  is(
    'the people list carries faces too',
    await ryan.page.evaluate(() =>
      [...document.querySelectorAll('.strip')].map((s) => ({
        who: s.querySelector('.label')?.textContent.replace(/\s*\(you\)$/, '').trim(),
        drawn: s.querySelector('.face img')?.naturalWidth ?? 0,
        // Small, because it sits on one line of 14px text.
        px: Math.round(s.querySelector('.face')?.getBoundingClientRect().width ?? 0),
      })),
    ),
    // Only who is actually in the room -- Alex was invited and never
    // opened a tab, so she is on nobody's strip.
    [
      { who: 'Ryan', drawn: 8, px: 22 },
      { who: 'Vox', drawn: 8, px: 22 },
    ],
  );

  // And a face in that list opens the same card as one in the
  // conversation: clicking a face is one thing, wherever it is.
  await ryan.page.click('.strip .face');
  await ryan.page.waitForSelector('.who-card');
  is(
    'and opens the same card from there',
    await ryan.page.evaluate(() => document.querySelector('.who-card h2').textContent.trim()),
    'Ryan',
  );
  await ryan.page.keyboard.press('Escape');
  await wait(200);

  // Yours to take off, from the one place that offers it. The room
  // tells every tab the moment it goes, so the square in the
  // conversation behind this page turns back into initials without a
  // reload -- which is the half a DELETE on its own wouldn't prove.
  await ryan.page.click('.tools button.cog');
  await ryan.page.waitForSelector('.settings');
  await wait(400);

  // The ones worn before, newest first, with the current one marked.
  is(
    'Settings offers the faces worn before',
    await ryan.page.evaluate(() =>
      [...document.querySelectorAll('.past-face')].map((b) => ({
        asked: b.getAttribute('title'),
        worn: b.getAttribute('aria-pressed') === 'true',
      })),
    ),
    [
      { asked: 'a wizard at ease', worn: true },
      { asked: 'a rested wizard', worn: false },
      { asked: 'a tired wizard', worn: false },
      { asked: 'a wizard at full size', worn: false },
    ],
  );

  // And each of them is drawn the way its own picture wants. A
  // picture server draws in more than one look -- the sprites here
  // are 8 pixels and the last one is 64, in a 56-pixel square -- so
  // the row holds both kinds at once: the small ones blown up pixel
  // by pixel, the big one shrunk with smoothing. Magnifying that one
  // would throw away most of it and leave noise where a face was.
  await wait(300);
  is(
    'and each is drawn the way its own picture wants',
    await ryan.page.evaluate(() =>
      [...document.querySelectorAll('.past-face img')].map(
        (img) => `${img.naturalWidth}:${getComputedStyle(img).imageRendering}`,
      ),
    ),
    ['8:pixelated', '8:pixelated', '8:pixelated', '64:auto'],
  );

  // Putting an older one back on costs nothing and needs no AI.
  await ryan.page.evaluate(() => document.querySelectorAll('.past-face')[2].click());
  await wait(600);
  is(
    'and an older one can be put back on',
    await ryan.page.evaluate(() =>
      [...document.querySelectorAll('.past-face')].map(
        (b) => b.getAttribute('aria-pressed') === 'true',
      ),
    ),
    [false, false, true, false],
  );

  await ryan.page.screenshot({ path: '/tmp/vox-room-settings.png' });

  is(
    'Settings offers to take your face off',
    await ryan.page.evaluate(() =>
      [...document.querySelectorAll('.settings button')].some(
        (b) => b.textContent.trim() === 'Take it off',
      ),
    ),
    true,
  );
  await ryan.page.evaluate(() =>
    [...document.querySelectorAll('.settings button')]
      .find((b) => b.textContent.trim() === 'Take it off')
      .click(),
  );
  await wait(500);
  is(
    'and it is gone, with the initials back in its place',
    await ryan.page.evaluate(() => {
      const face = document.querySelector('.settings .face-row .face');
      return { img: !!face?.querySelector('img'), initials: face?.querySelector('.initials')?.textContent.trim() };
    }),
    { img: false, initials: 'R' },
  );

  // And in the conversation, which nobody reloaded.
  await ryan.page.click('.settings header button.primary');
  await ryan.page.waitForSelector('.said');
  is(
    'including beside what they said',
    await ryan.page.evaluate(() => {
      const mine = [...document.querySelectorAll('.said > .entry')].find(
        (e) => e.querySelector('.name')?.textContent.trim() === 'Ryan',
      );
      return !!mine.querySelector(':scope > .face img');
    }),
    false,
  );

  await ryan.browser.close();
  report();
})();
