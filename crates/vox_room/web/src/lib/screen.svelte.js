// Desktop sharing. Its own WebSocket (`/screen.ws`), separate from the
// room's, so a burst of video can't sit in front of a 20 ms frame of
// somebody talking.
//
// Sharing encodes the same picture twice with WebCodecs: a thumbnail
// (small, a couple of frames a second) that everyone is always sent for
// the mixer, and the full size, only encoded while the server says
// somebody has it open. Watching is this tab's own business -- opening a
// desktop opens it for nobody else, and nothing here is in the room's
// shared state.

const THUMB = 0;
const FULL = 1;
const HEADER = 12;
const KEY = 1;

// The thumbnail in the mixer: wide enough to recognise a window, cheap
// enough to send to everyone all the time.
const THUMB_W = 320;
const THUMB_FPS = 2;
// The full size. A desktop is mostly still, so the frame rate is low and
// the keyframes are what cost.
const FULL_W = 1920;
const FULL_FPS = 10;
const FULL_BITRATE = 2_500_000;
const THUMB_BITRATE = 150_000;
// A keyframe this often even if nobody asked, so a tab that arrives
// between requests still fills in quickly.
const KEY_EVERY_MS = 2000;

export const screen = $state({
  connected: false,
  // Who is sharing: { id, person, codec, thumb: {w,h}, full: {w,h} }.
  // Everyone sees the same list -- it is who is sharing, not who is
  // watching.
  streams: [],
  // This tab is sharing its own desktop.
  sharing: false,
  // The stream this tab has open full size, and nobody else's business.
  watching: null,
  error: null,
});

export function supported() {
  return typeof window.VideoEncoder === 'function'
    && typeof window.VideoDecoder === 'function'
    && !!navigator.mediaDevices?.getDisplayMedia;
}

/// The stream somebody is sharing, or undefined.
export function streamOf(person) {
  return screen.streams.find((s) => s.person === person);
}

let ws = null;
let retry = 0;
// Canvases to draw each stream's thumbnail into, by stream id. A
// component registers one when it mounts and drops it when it goes.
const thumbs = new Map();
// The canvas for whatever is open full size, if the viewer is showing one.
let board = null;
// VideoDecoders, keyed `${id}:${kind}`.
const decoders = new Map();
let publisher = null;

export function connect() {
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${scheme}://${location.host}/screen.ws`);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    screen.connected = true;
    retry = 0;
    // A socket that dropped and came back: say again what we were doing.
    if (publisher) publisher.announce();
    if (screen.watching != null) send({ type: 'watch', id: screen.watching });
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') apply(JSON.parse(ev.data));
    else frame(new Uint8Array(ev.data));
  };
  ws.onclose = () => {
    screen.connected = false;
    // The decoders are of streams that may be gone, or resized, by the
    // time we are back: the server sends a keyframe to start again with.
    for (const d of decoders.values()) close(d);
    decoders.clear();
    setTimeout(connect, Math.min(500 * 2 ** retry++, 5000));
  };
}

function send(msg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function close(d) {
  try { d.close(); } catch {}
}

function apply(ev) {
  switch (ev.type) {
    case 'streams': {
      const before = new Map(screen.streams.map((s) => [s.id, s]));
      screen.streams = ev.streams;
      // A stream that stopped, or came back a different size, has
      // decoders configured for a picture that no longer exists.
      for (const s of ev.streams) {
        const was = before.get(s.id);
        if (was && (was.codec !== s.codec || was.full.w !== s.full.w || was.full.h !== s.full.h
          || was.thumb.w !== s.thumb.w || was.thumb.h !== s.thumb.h)) {
          drop(s.id);
        }
        before.delete(s.id);
      }
      for (const id of before.keys()) drop(id);
      // What this tab was watching has stopped.
      if (screen.watching != null && !ev.streams.some((s) => s.id === screen.watching)) {
        screen.watching = null;
      }
      break;
    }
    case 'publishing':
      if (publisher) publisher.id = ev.id;
      break;
    case 'keyframe':
      publisher?.keyframe(ev.kind);
      break;
    case 'want':
      publisher?.want(ev.full);
      break;
  }
}

/// Forget the decoders for one stream.
function drop(id) {
  for (const kind of [THUMB, FULL]) {
    const d = decoders.get(`${id}:${kind}`);
    if (d) {
      close(d);
      decoders.delete(`${id}:${kind}`);
    }
  }
}

function frame(msg) {
  if (msg.length <= HEADER) return;
  const kind = msg[0];
  const key = (msg[1] & KEY) !== 0;
  const view = new DataView(msg.buffer, msg.byteOffset, msg.byteLength);
  const id = view.getUint16(2, true);
  const ts = Number(view.getBigUint64(4, true));
  const stream = screen.streams.find((s) => s.id === id);
  // A frame of something we haven't been told about, or of the full size
  // after this tab closed it.
  if (!stream) return;
  if (kind === FULL && screen.watching !== id) return;

  const at = `${id}:${kind}`;
  let dec = decoders.get(at);
  if (dec && dec.state === 'closed') {
    decoders.delete(at);
    dec = null;
  }
  if (!dec) {
    // Nothing can be decoded from a delta on its own: the server only
    // sends one once a keyframe has reached us, but a socket that just
    // came back may still have one in flight.
    if (!key) return;
    dec = new VideoDecoder({
      output: (f) => paint(id, kind, f),
      error: (e) => {
        console.warn('[screen] decoder', e);
        drop(id);
      },
    });
    const size = kind === THUMB ? stream.thumb : stream.full;
    dec.configure({ codec: stream.codec, codedWidth: size.w, codedHeight: size.h,
      optimizeForLatency: true });
    decoders.set(at, dec);
  }
  if (dec.state !== 'configured') return;
  try {
    dec.decode(new EncodedVideoChunk({
      type: key ? 'key' : 'delta',
      timestamp: ts,
      data: msg.subarray(HEADER),
    }));
  } catch (e) {
    console.warn('[screen] decode', e);
    drop(id);
  }
}

/// One decoded picture onto whichever canvas is showing it.
function paint(id, kind, frame) {
  try {
    const canvas = kind === THUMB ? thumbs.get(id) : board;
    if (!canvas) return;
    if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
      canvas.width = frame.displayWidth;
      canvas.height = frame.displayHeight;
    }
    canvas.getContext('2d')?.drawImage(frame, 0, 0);
  } finally {
    frame.close();
  }
}

/// A component showing one stream's thumbnail. Returns its undo.
export function showThumb(id, canvas) {
  thumbs.set(id, canvas);
  return () => {
    if (thumbs.get(id) === canvas) thumbs.delete(id);
  };
}

/// Open one desktop full size, on this screen alone. `null` closes it.
export function watch(id) {
  if (id != null && !screen.streams.some((s) => s.id === id)) return;
  if (screen.watching === id) return;
  if (screen.watching != null) drop(screen.watching);
  screen.watching = id;
  send({ type: 'watch', id });
}

export function unwatch() {
  if (screen.watching != null) drop(screen.watching);
  board = null;
  screen.watching = null;
  send({ type: 'watch', id: null });
}

/// The canvas the full-size view draws into, once it has mounted.
export function showBoard(canvas) {
  board = canvas;
  return () => {
    if (board === canvas) board = null;
  };
}

// -- sharing this desktop -------------------------------------------------

/// Two encoders over one captured desktop: the thumbnail everyone gets,
/// and the full size only while somebody has it open.
class Publisher {
  constructor(stream, onEnd) {
    this.stream = stream;
    this.onEnd = onEnd;
    this.video = Object.assign(document.createElement('video'), {
      srcObject: stream,
      muted: true,
      playsInline: true,
    });
    // A <video> the document hasn't got may never load its metadata,
    // and so never have a size or a frame to draw: keep it in the page,
    // two pixels, off the side of it.
    Object.assign(this.video.style, {
      position: 'fixed',
      left: '-10000px',
      top: '0',
      width: '2px',
      height: '2px',
      opacity: '0',
      pointerEvents: 'none',
    });
    // Both encoders take their frames from a canvas rather than the
    // track, so the one capture can feed two sizes and a window that
    // changes size is just a different draw.
    this.canvas = { [THUMB]: new OffscreenCanvas(2, 2), [FULL]: new OffscreenCanvas(2, 2) };
    this.enc = {};
    this.size = {};
    this.wanted = false;
    this.ts = 0;
    this.need = new Set([THUMB]);
    this.lastKey = { [THUMB]: 0, [FULL]: 0 };
    this.ticks = 0;
    this.source = { w: 0, h: 0 };
  }

  async start() {
    document.body.appendChild(this.video);
    await this.video.play();
    for (const t of this.stream.getVideoTracks()) {
      // Sharpness over smoothness: this is a desktop, with text on it.
      t.contentHint = 'text';
      // The browser's own "Stop sharing" button, which is not ours.
      t.addEventListener('ended', () => this.onEnd());
    }
    this.timer = setInterval(() => this.tick(), 1000 / FULL_FPS);
  }

  /// What to encode the two pictures at, given the desktop's own size.
  plan() {
    const w = this.video.videoWidth;
    const h = this.video.videoHeight;
    if (!w || !h) return false;
    if (w === this.source.w && h === this.source.h) return true;
    this.source = { w, h };
    // Even dimensions, and no bigger than the source.
    const fit = (target) => {
      const scale = Math.min(1, target / w);
      return { w: Math.max(16, Math.round((w * scale) / 2) * 2),
               h: Math.max(16, Math.round((h * scale) / 2) * 2) };
    };
    this.size = { [THUMB]: fit(THUMB_W), [FULL]: fit(FULL_W) };
    for (const kind of [THUMB, FULL]) {
      const { w: cw, h: ch } = this.size[kind];
      this.canvas[kind].width = cw;
      this.canvas[kind].height = ch;
      if (this.enc[kind]) close(this.enc[kind]);
      this.enc[kind] = new VideoEncoder({
        output: (chunk) => this.chunk(kind, chunk),
        error: (e) => {
          console.error('[screen] encoder', e);
          screen.error = e?.message || String(e);
          this.onEnd();
        },
      });
      this.enc[kind].configure({
        codec: 'vp8',
        width: cw,
        height: ch,
        bitrate: kind === THUMB ? THUMB_BITRATE : FULL_BITRATE,
        framerate: kind === THUMB ? THUMB_FPS : FULL_FPS,
        latencyMode: 'realtime',
      });
      this.need.add(kind);
    }
    this.announce();
    return true;
  }

  /// Tell the room what it is about to be sent. Also what a reconnected
  /// socket says to pick up where it left off.
  announce() {
    if (!this.size[FULL]) return;
    send({ type: 'publish', codec: 'vp8', thumb: this.size[THUMB], full: this.size[FULL] });
  }

  /// The relay wants a keyframe for one of the two pictures.
  keyframe(kind) {
    this.need.add(kind);
  }

  /// Whether anybody has the full size open. Nobody: don't encode it.
  want(on) {
    this.wanted = on;
  }

  tick() {
    if (!this.plan()) return;
    this.ticks++;
    const now = performance.now();
    const kinds = [];
    // The thumbnail runs slower than the full size, so only some ticks
    // are its own.
    if (this.ticks % Math.round(FULL_FPS / THUMB_FPS) === 0) kinds.push(THUMB);
    if (this.wanted) kinds.push(FULL);
    // Even with nobody watching, a thumbnail keyframe is owed now and
    // then so a tab that just arrived fills in.
    for (const kind of kinds) {
      const enc = this.enc[kind];
      if (!enc || enc.state !== 'configured') continue;
      if (enc.encodeQueueSize > 2) continue; // behind: skip, don't pile up
      const key = this.need.delete(kind) || now - this.lastKey[kind] > KEY_EVERY_MS;
      if (key) this.lastKey[kind] = now;
      const { w, h } = this.size[kind];
      this.canvas[kind].getContext('2d').drawImage(this.video, 0, 0, w, h);
      const frame = new VideoFrame(this.canvas[kind], { timestamp: this.ts });
      try {
        enc.encode(frame, { keyFrame: key });
      } catch (e) {
        console.warn('[screen] encode', e);
      } finally {
        frame.close();
      }
    }
    this.ts += Math.round(1e6 / FULL_FPS);
    // The sharer's own thumbnail: drawn here rather than sent to the
    // server and back.
    const mine = thumbs.get(this.id);
    if (mine) {
      const { w, h } = this.size[THUMB];
      if (mine.width !== w || mine.height !== h) {
        mine.width = w;
        mine.height = h;
      }
      mine.getContext('2d')?.drawImage(this.canvas[THUMB], 0, 0);
    }
  }

  chunk(kind, chunk) {
    if (ws?.readyState !== WebSocket.OPEN) return;
    const body = new Uint8Array(HEADER + chunk.byteLength);
    const view = new DataView(body.buffer);
    body[0] = kind;
    body[1] = chunk.type === 'key' ? KEY : 0;
    view.setBigUint64(4, BigInt(Math.max(0, Math.round(chunk.timestamp))), true);
    chunk.copyTo(body.subarray(HEADER));
    ws.send(body);
  }

  stop() {
    clearInterval(this.timer);
    for (const t of this.stream.getTracks()) t.stop();
    for (const kind of [THUMB, FULL]) if (this.enc[kind]) close(this.enc[kind]);
    this.video.remove();
    this.video.srcObject = null;
  }
}

/// Share this desktop. The browser asks which window or screen.
export async function share() {
  if (publisher) return;
  screen.error = null;
  if (!supported()) {
    screen.error = "This browser can't share a desktop (it has no WebCodecs video).";
    return;
  }
  let stream;
  try {
    stream = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: FULL_FPS },
      // The room's sound is the mixer's: a shared tab's audio would
      // come back as another voice to transcribe.
      audio: false,
    });
  } catch (e) {
    // Cancelling the browser's own picker is not an error worth saying.
    if (e?.name !== 'NotAllowedError' && e?.name !== 'AbortError') {
      screen.error = e?.message || String(e);
    }
    return;
  }
  publisher = new Publisher(stream, () => stopSharing());
  try {
    await publisher.start();
    screen.sharing = true;
  } catch (e) {
    console.error('[screen] share', e);
    screen.error = e?.message || String(e);
    publisher.stop();
    publisher = null;
  }
}

export function stopSharing() {
  if (!publisher) return;
  publisher.stop();
  publisher = null;
  screen.sharing = false;
  send({ type: 'unpublish' });
}
