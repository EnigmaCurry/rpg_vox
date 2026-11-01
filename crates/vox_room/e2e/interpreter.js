// The interpreter is only there while it has work, and how loud it is
// is yours:
//   node interpreter.js RYAN_LINK ALEX_LINK
//
// An interpretation is heard by whoever is listening in that language
// and by nobody else, so there is no shared fader for it: turning yours
// down does nothing to anybody else's. It appears only while somebody in
// the room speaks a language you don't read, and a room of your own can
// never have one.
//
// Wants a room with two languages, Alex set to the second. A Qwen3 voice
// and a translator are only HTTP clients, so they need an address and
// not a server for any of this:
//
//   vox_room --room Lobby --room Studio --no-stt \
//     --tts qwen3 --tts-url http://127.0.0.1:1 \
//     --model fake --llm-url http://127.0.0.1:1/v1 \
//     --language English --language-also Russian --translate openai

const { is, report, person, strips } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);

const interpreter = (page) =>
  page.evaluate(() => {
    const s = [...document.querySelectorAll('.room.here .strip')].find(
      (s) => s.querySelector('.label').textContent.trim() === 'Interpreter',
    );
    return s
      ? { fader: Number(s.querySelector('input[type=range]').value), doing: s.querySelector('.state').textContent.trim() }
      : null;
  });

(async () => {
  const ryan = await person(ryanLink);
  is('alone in the room, there is nothing to interpret',
    await strips(ryan.page), ['Ryan (you)', 'Vox']);

  // Alex comes in, and she reads in Russian.
  const alex = await person(alexLink);
  await ryan.page.waitForTimeout(900);
  is('somebody who reads another language is here',
    await strips(ryan.page), ['Alex', 'Ryan (you)', 'Vox', 'Interpreter']);
  is('and she has one too',
    await strips(alex.page), ['Alex (you)', 'Ryan', 'Vox', 'Interpreter']);

  // Turning it down is Ryan's business and nobody else's.
  await ryan.page.evaluate(() => {
    const s = [...document.querySelectorAll('.room.here .strip')].find(
      (s) => s.querySelector('.label').textContent.trim() === 'Interpreter',
    );
    const r = s.querySelector('input[type=range]');
    r.value = '0';
    r.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await ryan.page.waitForTimeout(700);
  is('Ryan turned his own down', (await interpreter(ryan.page)).fader, 0);
  is('and his says so', (await interpreter(ryan.page)).doing, 'off');
  is("Alex's is where she left it", (await interpreter(alex.page)).fader, 1);

  // A room of your own never has one: there is nobody to interpret to.
  await ryan.page.click('.rooms button:has-text("You and Vox")');
  await ryan.page.waitForTimeout(1200);
  is('none in a room of your own', await strips(ryan.page), ['Ryan (you)', 'Vox']);

  // And when the other language leaves, it goes.
  await alex.browser.close();
  await ryan.page.click('.rooms button:has-text("Lobby")');
  await ryan.page.waitForTimeout(1500);
  is('nobody left to interpret for', await strips(ryan.page), ['Ryan (you)', 'Vox']);

  await ryan.browser.close();
  report();
})();
