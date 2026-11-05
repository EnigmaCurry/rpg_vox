// The door: a page of its own, a thing you agree to, and a language
// already filled in.
//   node door.js FRESH_LINK KNOWN_LINK EN_LINK RU_LINK FR_LINK
//
// Checks that nothing of the room is visible or even connected from out
// here, that Join Audio does nothing until you agree to be recorded,
// that agreeing is remembered and can be taken back, and that the
// Language box is never blank — the system's language the first time,
// the room's own when it doesn't speak it.
//
// The last three links want a room with two languages in it; the first
// two work in any room.

const { is, report, atTheDoor, comeIn, AGREE, JOIN } = require('./room.js');
const [fresh, known, en, ru, fr] = process.argv.slice(2);

/// What the Language box has in it, if the room offers a choice.
const language = (page) =>
  page.evaluate(() => {
    const s = [...document.querySelectorAll('.enter label')]
      .find((l) => l.querySelector('span')?.textContent.trim() === 'Language')
      ?.querySelector('select');
    return s ? s.value : null;
  });

const stored = (page, key) => page.evaluate((k) => localStorage.getItem(k), key);

(async () => {
  // Somebody the room already knows: the one most likely to have a
  // conversation to leak behind the card.
  const k = await atTheDoor(known);
  // On record before coming in, so the reload below can be asked where
  // the box got its value.
  await k.page.selectOption('.enter label:has(span:text-is("Language")) select', 'Russian');
  await k.page.waitForTimeout(500);
  await comeIn(k.page);
  await k.page.evaluate(() => document.querySelector('.tools button.leave').click());
  await k.page.waitForTimeout(900);

  is(
    'no room behind the door',
    await k.page.evaluate(() => ({
      app: !!document.querySelector('.app'),
      log: !!document.querySelector('.log'),
      rooms: !!document.querySelector('.rooms'),
    })),
    { app: false, log: false, rooms: false },
  );
  is(
    'and nothing to blur, because there is nothing behind it',
    await k.page.evaluate(() => getComputedStyle(document.querySelector('.enter')).backdropFilter),
    'none',
  );
  // It used to say "room" four times before it got to the microphone:
  // an eyebrow over a heading that said the same, and a lead that said
  // it twice more. The heading and the lead say it once each now, which
  // is the copy that was asked for.
  is(
    'no eyebrow repeating the heading',
    await k.page.evaluate(() => !!document.querySelector('.enter .eyebrow')),
    false,
  );
  is(
    'and the lead says it once',
    await k.page.evaluate(
      () => (document.querySelector('.enter .lead').textContent.match(/room/gi) ?? []).length,
    ),
    1,
  );
  is(
    'and who is in it is not the door’s to say',
    await k.page.evaluate(() => !!document.querySelector('.enter .who')),
    false,
  );

  // A browser the room already knows has no socket out here either, and
  // the Language box used to be missing entirely for it -- there until
  // you joined once, gone again on the next reload. The room's own
  // record is what fills it, so this clears what the browser remembered
  // and asks again.
  await k.page.evaluate(() => localStorage.removeItem('vox_room.lang'));
  await k.page.reload();
  await k.page.waitForSelector('.enter');
  await k.page.waitForTimeout(900);
  is('a browser the room knows is offered the language too', await language(k.page), 'Russian');
  is('and it came from the room, not from this browser',
    await stored(k.page, 'vox_room.lang'), 'Russian');

  await k.browser.close();

  // A fresh invite, and nothing agreed to yet.
  const f = await atTheDoor(fresh);
  const sockets = [];
  f.page.on('websocket', (w) => sockets.push(w.url()));
  await f.page.waitForTimeout(600);
  is('the door opens no socket', sockets, []);
  is('an invite still names you', await f.page.evaluate(() => !!document.querySelector('.enter .name input')), true);
  is('nothing agreed yet', await f.page.locator(AGREE).isChecked(), false);
  is('and the way in is shut', await f.page.locator(JOIN).isDisabled(), true);
  is(
    'it says what it is asking about',
    await f.page.evaluate(() => /transcri(bed|ption)/.test(document.querySelector('.consent p').textContent)),
    true,
  );

  await f.page.locator(AGREE).check();
  is('agreed, and written down', await stored(f.page, 'vox_room.consent'), 'on');
  is('and the way in opens', await f.page.locator(JOIN).isDisabled(), false);

  // Taking it back shuts it again.
  await f.page.locator(AGREE).uncheck();
  is('taken back', await stored(f.page, 'vox_room.consent'), 'off');
  is('and it is shut again', await f.page.locator(JOIN).isDisabled(), true);

  await f.page.locator(AGREE).check();
  await f.page.click(JOIN);
  await f.page.waitForTimeout(2500);
  is('in the room', await f.page.evaluate(() => !!document.querySelector('.app')), true);
  is('and now there are sockets', sockets.length >= 2, true);

  // Out again: the sockets go with the audio, and so does everything
  // they told us.
  await f.page.evaluate(() => document.querySelector('.tools button.leave').click());
  await f.page.waitForTimeout(1200);
  is('back at the door', await f.page.evaluate(() => !!document.querySelector('.enter')), true);
  is(
    'with nothing of the room left',
    await f.page.evaluate(() => !document.querySelector('.app') && !document.querySelector('.rooms')),
    true,
  );
  is('and no asking twice', await f.page.locator(AGREE).isChecked(), true);
  await f.page.reload();
  await f.page.waitForSelector('.enter');
  await f.page.waitForTimeout(900);
  is('agreed across a reload too', await f.page.locator(AGREE).isChecked(), true);
  await f.browser.close();

  // The Language box is never blank: the system's, when the room speaks
  // it, and the room's own when it doesn't.
  for (const [link, locale, want] of [
    [en, 'en-US', 'English'],
    [ru, 'ru-RU', 'Russian'],
    [fr, 'fr-FR', 'English'],
  ]) {
    if (!link) continue;
    const d = await atTheDoor(link, { locale });
    is(`a ${locale} system gets ${want}`, await language(d.page), want);
    is('and it is written down, so it is only guessed once',
      await stored(d.page, 'vox_room.lang'), want);
    await d.browser.close();
  }

  report();
})();
