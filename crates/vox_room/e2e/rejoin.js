// Leaving the audio and coming back in.
//
// Out is out: the sockets go with the audio, so somebody who left is on
// nobody's roster and the room hears nothing from them. Coming back is
// the same door again, and what matters is that it is a whole way back
// -- the socket, the microphone and the mix -- rather than a page that
// merely stops saying it is at the door.
//
// So this is measured from the other side of the room rather than from
// the page that left: the mixer writes a level for everybody in the
// room ten times a second, and an input that has run dry is padded with
// exactly zero where a live microphone never is.
const { person, socket, wait, until, is, report, JOIN } = require('./room.js');
const [ryanLink, alexLink] = process.argv.slice(2);
const token = (l) => l.replace(/.*\/join\//, '');
const WHO = 'Ryan';

(async () => {
  // Alex only watches, on a plain socket: no browser, no audio.
  const alex = await socket(token(alexLink));
  let peak = 0;
  const base = alex.ws.onmessage;
  alex.ws.onmessage = (ev) => {
    base(ev);
    if (typeof ev.data !== 'string') return;
    const m = JSON.parse(ev.data);
    if (m.type === 'levels' && m.levels && WHO in m.levels) peak = Math.max(peak, m.levels[WHO]);
  };
  /// The loudest the room heard from them over a second and a half.
  const heard = async () => { peak = 0; await wait(1500); return peak; };

  // No wav: Chromium's own test tone is enough to move a meter.
  const ryan = await person(ryanLink);
  await until(() => alex.people.includes(WHO));
  is('the room has him', alex.people.includes(WHO), true);
  is('and can hear him', (await heard()) > 0, true);

  await ryan.page.click('.tools button[aria-label="Leave audio"]');
  await ryan.page.waitForSelector(JOIN);
  await until(() => !alex.people.includes(WHO));
  is('leaving takes him off the roster', alex.people.includes(WHO), false);
  is('and the room goes quiet where he was', await heard(), 0);

  await ryan.page.click(JOIN);
  await until(() => alex.people.includes(WHO));
  is('the same door lets him back in', alex.people.includes(WHO), true);
  // The whole point: a page that has stopped saying it is at the door
  // has not necessarily opened a microphone.
  is('with his microphone open again', (await heard()) > 0, true);

  await ryan.browser.close();
  alex.ws.close();
  report();
})();
