// The room's audio loop, kept off the page's own thread.
//
// This worker owns `/room.ws` and both halves of the codec: the frames
// the mic worklet hands over are Opus-encoded here and put on the
// socket, and the mix that comes back is decoded here and handed to the
// player worklet. The page is sent only what it draws -- the room's
// events, and a meter ten times a second.
//
// That division is the whole point. A 20 ms frame in either direction
// used to pass through the main thread, which is also the thread
// rendering a live transcript, and on a phone with the screen off it is
// the first thing the browser throttles. Nothing on the audio path is
// on it now.
//
// The socket is here for the same reason and not only because the
// frames arrive on it: a socket that drops wants reconnecting promptly,
// and the timer that waits out the backoff would otherwise be a timer
// on the one thread that cannot be trusted to keep time.

const RATE = 48000;
const FRAME_US = 20000;

let ws = null;
/// Whether there is supposed to be a socket. One that drops comes back;
/// one that was shut on the way out to the door does not.
let wanted = false;
let base = '';
/// Which room to ask for when reconnecting. See the page's `connect`:
/// a tab whose socket dropped says where it had got to.
let seat = '';
let retry = 0;
let timer = null;

let decoder = null;
let encoder = null;
/// The port into the player worklet, while there is audio to play.
let player = null;
let inTs = 0;
let outTs = 0;
let muted = false;

function connect() {
  clearTimeout(timer);
  timer = null;
  ws = new WebSocket(base + (seat ? `?room=${encodeURIComponent(seat)}` : ''));
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    retry = 0;
    postMessage({ type: 'up' });
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data !== 'string') return frame(ev.data);
    // Parsed here rather than on the page: the page is handed the event
    // it is going to act on, and none of the work of getting it.
    try {
      postMessage({ type: 'event', ev: JSON.parse(ev.data) });
    } catch (e) {
      console.warn('[room] event', e);
    }
  };
  ws.onclose = () => {
    postMessage({ type: 'down' });
    // Shut on purpose: don't crawl back.
    if (!wanted) return;
    timer = setTimeout(connect, Math.min(500 * 2 ** retry++, 5000));
  };
}

function makeDecoder() {
  inTs = 0;
  decoder = new AudioDecoder({
    output: (data) => {
      const pcm = new Float32Array(data.numberOfFrames);
      try {
        data.copyTo(pcm, { planeIndex: 0, format: 'f32-planar' });
      } finally {
        data.close();
      }
      player?.postMessage(pcm, [pcm.buffer]);
    },
    error: (e) => console.error('[room] decoder', e),
  });
  decoder.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1 });
}

/// One Opus frame of this tab's mix, off the socket. Frames that arrive
/// before there is anything to play them through are dropped: the
/// socket opens on the way into the room and the speakers follow it.
function frame(bytes) {
  if (!player) return;
  // A decoder that hit an error is closed for good: make a new one.
  if (decoder?.state === 'closed') {
    console.warn('[room] decoder closed; starting a new one');
    makeDecoder();
  }
  if (decoder?.state !== 'configured') return;
  try {
    decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: inTs, duration: FRAME_US, data: bytes }));
    inTs += FRAME_US;
  } catch (e) {
    console.warn('[room] decode', e);
  }
}

/// One 20 ms frame of this person's mic, up from its worklet.
function mic(pcm) {
  // Muted still sends silence: the room's clock and transcription keep
  // going, and nobody hears a hitch when you unmute.
  if (muted) pcm.fill(0);
  if (encoder?.state !== 'configured') return;
  const data = new AudioData({
    format: 'f32-planar',
    sampleRate: RATE,
    numberOfChannels: 1,
    numberOfFrames: pcm.length,
    timestamp: outTs,
    data: pcm,
  });
  outTs += FRAME_US;
  try {
    encoder.encode(data);
  } finally {
    data.close();
  }
}

function openMic(port) {
  outTs = 0;
  encoder = new AudioEncoder({
    output: (chunk) => {
      const buf = new Uint8Array(chunk.byteLength);
      chunk.copyTo(buf);
      if (ws?.readyState === WebSocket.OPEN) ws.send(buf);
    },
    error: (e) => console.error('[room] encoder', e),
  });
  encoder.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1, bitrate: 64000, opus: { frameDuration: 20000 } });
  port.onmessage = (ev) => {
    if (ev.data instanceof Float32Array) mic(ev.data);
  };
}

function closeMic() {
  try { encoder?.close(); } catch {}
  encoder = null;
}

onmessage = (ev) => {
  const m = ev.data;
  switch (m.type) {
    case 'connect':
      base = m.base;
      seat = m.seat || '';
      wanted = true;
      connect();
      break;
    case 'seat':
      seat = m.seat || '';
      break;
    case 'reconnect': {
      const old = ws;
      ws = null;
      if (old) {
        // Ours to replace, so the drop handler must not also crawl
        // back: two sockets would both be in the room, under the same
        // name. The page is not told it went down, either -- it didn't,
        // as far as anything it draws is concerned.
        old.onclose = null;
        old.close();
      }
      connect();
      break;
    }
    case 'send':
      if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m.msg));
      break;
    case 'player':
      player = m.port;
      makeDecoder();
      break;
    case 'mic':
      openMic(m.port);
      break;
    case 'nomic':
      closeMic();
      break;
    case 'muted':
      muted = m.on;
      break;
    case 'silence':
      closeMic();
      try { decoder?.close(); } catch {}
      decoder = null;
      player = null;
      break;
    case 'shut':
      wanted = false;
      clearTimeout(timer);
      timer = null;
      ws?.close();
      ws = null;
      break;
  }
};
