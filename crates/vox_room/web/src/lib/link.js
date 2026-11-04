// The page's handle on the room's socket, which lives in a worker along
// with the codec (see audio.worker.js).
//
// Everything here is a message. The page asks for a socket, puts
// commands on it, and is told what came back; the audio it only
// introduces, by handing over the two ports the worker talks to the
// worklets through. What the page never does is touch a frame.

export class Link {
  /// `onEvent` is handed each of the room's events, already parsed.
  /// `onUp`/`onDown` are the socket arriving and going away -- a drop
  /// the worker is already crawling back from, so they say whether the
  /// room is reachable, not whether to do anything about it.
  constructor({ onEvent, onUp, onDown } = {}) {
    this.worker = new Worker(new URL('./audio.worker.js', import.meta.url), { type: 'module' });
    /// Whether there is a socket to send on right now.
    this.up = false;
    /// The loudest the mix has been in the last tenth of a second, as
    /// the worker last reported it. Read by the meter; see `heard`.
    this.peak = 0;
    this.worker.onmessage = (ev) => {
      const m = ev.data;
      if (m.type === 'event') onEvent?.(m.ev);
      else if (m.type === 'heard') this.peak = m.peak;
      else if (m.type === 'up') {
        this.up = true;
        onUp?.();
      } else if (m.type === 'down') {
        this.up = false;
        onDown?.();
      }
    };
  }

  /// Open the socket, and keep it open: the worker reconnects on its own
  /// until `shut`.
  connect(seat = '') {
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
    this.worker.postMessage({
      type: 'connect',
      base: `${scheme}://${location.host}/room.ws`,
      seat,
    });
  }

  /// Where this tab is sitting, for when the socket has to be made
  /// again. The worker asks for the room it was in rather than being
  /// put in the lobby and having to ask to be moved back.
  setSeat(seat) {
    this.worker.postMessage({ type: 'seat', seat: seat || '' });
  }

  /// Come back on a fresh socket, now rather than after the backoff a
  /// dropped one waits out. A socket is told whose it is when it opens
  /// and nothing afterwards changes its mind, so taking a new name needs
  /// a new one.
  ///
  /// The audio doesn't stop for it: the frames go out on whatever socket
  /// is current, the encoder and decoder are the worker's and untouched,
  /// and the `hello` that comes back says who you are now.
  reconnect() {
    this.worker.postMessage({ type: 'reconnect' });
  }

  send(msg) {
    this.worker.postMessage({ type: 'send', msg });
  }

  /// The loudest the mix has been lately. Not reset on reading: it is
  /// the last window the worker measured, and it falls to nothing on its
  /// own when the room goes quiet.
  heard() {
    return this.peak;
  }

  /// Hand the worker the port it plays the mix down. One end of a
  /// channel goes into the worklet and the other into the worker, so
  /// the decoded audio goes straight between them.
  usePlayer(node) {
    const { port1, port2 } = new MessageChannel();
    node.port.postMessage({ pcm: port1 }, [port1]);
    this.worker.postMessage({ type: 'player', port: port2 }, [port2]);
  }

  /// The same, the other way, for the mic.
  useMic(node) {
    const { port1, port2 } = new MessageChannel();
    node.port.postMessage({ pcm: port1 }, [port1]);
    this.worker.postMessage({ type: 'mic', port: port2 }, [port2]);
  }

  dropMic() {
    this.worker.postMessage({ type: 'nomic' });
  }

  setMuted(on) {
    this.worker.postMessage({ type: 'muted', on });
  }

  /// No more audio, but keep the socket: the room is still there to be
  /// talked to even when this tab has stopped listening to it.
  silence() {
    this.worker.postMessage({ type: 'silence' });
    // The worker stops measuring, so what it last reported would stand
    // for as long as the page kept asking. Nothing is being heard.
    this.peak = 0;
  }

  /// Shut the socket and stay shut. The worker itself stays: it is this
  /// page's one audio thread, and coming back into the room is a new
  /// socket rather than a new thread to put it on.
  shut() {
    this.worker.postMessage({ type: 'shut' });
    this.up = false;
  }
}
