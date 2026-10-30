//! Desktop sharing: a relay, and nothing it understands.
//!
//! `/screen.ws` is a second socket per tab, separate from `/room.ws` on
//! purpose. A burst of video in the audio socket's send queue is
//! head-of-line blocking applied to speech; on its own connection it
//! can't delay a 20 ms frame of somebody talking.
//!
//! A tab sharing its desktop publishes the same picture twice, VP8
//! encoded in the browser: a **thumbnail** (small, a couple of frames a
//! second), which everyone is always sent so the mixer can show it under
//! the sharer's name, and the **full** size, which is only encoded and
//! sent while somebody has it open. Who is watching what is each tab's
//! own business -- none of it is shared room state, and opening
//! somebody's desktop doesn't open it for anyone else.
//!
//! The server never decodes a frame. It forwards them, remembers who is
//! sharing, and keeps one rule: **a viewer is sent no delta frames until
//! a keyframe has reached them.** That is what makes dropping safe -- a
//! viewer who falls behind is put back to the next keyframe instead of
//! being shown a torn picture, and the publisher is asked for one.
//!
//! Every frame, both ways, is a 12-byte header and one encoded chunk:
//!
//! ```text
//! 0      kind: 0 thumbnail, 1 full size
//! 1      flags: bit 0 set on a keyframe
//! 2..4   u16 LE, which stream (filled in here; the browser sends zero)
//! 4..12  u64 LE, the frame's timestamp in microseconds
//! ```
//!
//! Text messages are JSON. From a tab:
//!
//! ```json
//! {"type": "publish", "codec": "vp8", "thumb": {"w":320,"h":180}, "full": {"w":1920,"h":1080}}
//! {"type": "unpublish"}
//! {"type": "watch", "id": 3}
//! {"type": "watch", "id": null}
//! ```
//!
//! And to one:
//!
//! ```json
//! {"type": "streams", "streams": [{"id":3,"person":"Alex","codec":"vp8","thumb":{…},"full":{…}}]}
//! {"type": "publishing", "id": 3}
//! {"type": "keyframe", "kind": 0}
//! {"type": "want", "full": true}
//! ```
//!
//! `want` is how a publisher learns whether to encode the full size at
//! all, and `keyframe` is the only thing the relay ever asks for.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Frames one viewer may have waiting. Past this they are behind, and
/// frames are dropped until a keyframe can start them again.
const VIEWER_QUEUE: usize = 16;
/// Control messages one tab may have waiting. They are small and rare --
/// who is sharing, a keyframe request -- so a tab that has filled this
/// is not keeping up with anything, and its socket is closed to make it
/// reconnect and start over.
const CTL_QUEUE: usize = 64;
/// The most an encoded frame may be. A desktop keyframe is a few hundred
/// kilobytes; this is only here so a bad socket can't ask for a gigabyte.
const FRAME_MAX: usize = 4 << 20;

/// Bytes before the encoded chunk in every frame, both ways.
const HEADER: usize = 12;
/// The small picture everyone is sent, for the mixer.
const THUMB: u8 = 0;
/// The full size, sent only to whoever has it open.
const FULL: u8 = 1;
/// `flags` bit 0.
const KEY: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    /// A picture a browser could actually have encoded.
    fn ok(self) -> bool {
        (16..=8192).contains(&self.w) && (16..=8192).contains(&self.h)
    }
}

/// A WebCodecs codec string, e.g. `vp8` or `vp09.00.10.08`.
fn codec_ok(codec: &str) -> bool {
    !codec.is_empty()
        && codec.len() <= 32
        && codec
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

struct Client {
    person: String,
    frames: mpsc::Sender<Vec<u8>>,
    ctl: mpsc::Sender<String>,
    /// The one full-size stream this tab has open, if any.
    watching: Option<u16>,
    /// Streams this tab must not be sent deltas for, because it has
    /// never had a keyframe or we dropped one of theirs.
    needs_key: HashSet<(u16, u8)>,
}

struct Publisher {
    id: u16,
    person: String,
    codec: String,
    thumb: Size,
    full: Size,
    /// Somebody has the full size open, so it is worth encoding.
    wanted: bool,
    /// Kinds we have already asked for a keyframe and not yet had, so a
    /// slow viewer doesn't ask once per frame.
    pending: HashSet<u8>,
}

#[derive(Default)]
struct State {
    clients: HashMap<u64, Client>,
    /// Keyed by the socket that is sharing: one tab, one desktop.
    publishers: HashMap<u64, Publisher>,
    next_id: u16,
}

pub struct Screen {
    st: Mutex<State>,
}

/// One frame's header, as the browser writes it. Only the tests build
/// one here -- the relay reads `kind` and the keyframe flag and patches
/// in the stream id, and leaves the rest alone.
#[cfg(test)]
fn header(kind: u8, key: bool, id: u16, ts: u64) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[0] = kind;
    h[1] = if key { KEY } else { 0 };
    h[2..4].copy_from_slice(&id.to_le_bytes());
    h[4..12].copy_from_slice(&ts.to_le_bytes());
    h
}

impl Screen {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            st: Mutex::new(State {
                next_id: 1,
                ..State::default()
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().expect("screen mutex poisoned")
    }

    /// A tab opened its screen socket. It is sent who is sharing at
    /// once, and the thumbnails start as soon as a keyframe can be had.
    pub fn add_client(
        &self,
        id: u64,
        person: &str,
    ) -> (mpsc::Receiver<Vec<u8>>, mpsc::Receiver<String>) {
        let (frames, frames_rx) = mpsc::channel(VIEWER_QUEUE);
        let (ctl, ctl_rx) = mpsc::channel(CTL_QUEUE);
        let mut st = self.lock();
        let needs_key = st
            .publishers
            .values()
            .map(|p| (p.id, THUMB))
            .collect::<HashSet<_>>();
        st.clients.insert(
            id,
            Client {
                person: person.to_string(),
                frames,
                ctl,
                watching: None,
                needs_key,
            },
        );
        let streams = streams_event(&st);
        send_ctl(&mut st, id, &streams);
        // Somebody new wants a picture, and the next keyframe of its own
        // accord could be a second or two away.
        let sockets: Vec<u64> = st.publishers.keys().copied().collect();
        for socket in sockets {
            ask_key(&mut st, socket, THUMB);
        }
        (frames_rx, ctl_rx)
    }

    pub fn remove_client(&self, id: u64) {
        let mut st = self.lock();
        forget(&mut st, id);
    }

    /// A tab is sharing its desktop. Called again when the shared window
    /// changes size, which keeps the stream's id and only changes what
    /// the viewers configure their decoders with.
    pub fn publish(&self, id: u64, codec: String, thumb: Size, full: Size) {
        // The codec string goes straight into every viewer's
        // `VideoDecoder.configure`, and the sizes into a canvas: a tab
        // may only say something that could be one.
        if !codec_ok(&codec) || !thumb.ok() || !full.ok() {
            debug!(socket = id, %codec, ?thumb, ?full, "implausible share");
            return;
        }
        let mut st = self.lock();
        if !st.clients.contains_key(&id) {
            return;
        }
        let person = st.clients[&id].person.clone();
        let stream = match st.publishers.get_mut(&id) {
            Some(p) => {
                if (&p.codec, p.thumb, p.full) == (&codec, thumb, full) {
                    return;
                }
                p.codec = codec;
                p.thumb = thumb;
                p.full = full;
                p.pending.clear();
                p.id
            }
            None => {
                let stream = st.next_id;
                st.next_id = st.next_id.wrapping_add(1).max(1);
                st.publishers.insert(
                    id,
                    Publisher {
                        id: stream,
                        person: person.clone(),
                        codec,
                        thumb,
                        full,
                        wanted: false,
                        pending: HashSet::new(),
                    },
                );
                info!(socket = id, person = %person, stream, "sharing a desktop");
                stream
            }
        };
        // Whatever the viewers had decoded is finished: everyone starts
        // again at the next keyframe, with the new size.
        for c in st.clients.values_mut() {
            c.needs_key.insert((stream, THUMB));
            c.needs_key.insert((stream, FULL));
        }
        // The sharer is told its own stream id, so its strip can draw
        // the picture it is encoding instead of waiting for a round trip
        // of it.
        send_ctl(&mut st, id, &json!({ "type": "publishing", "id": stream }));
        broadcast_streams(&mut st);
        wants(&mut st);
        ask_key(&mut st, id, THUMB);
    }

    /// A tab stopped sharing (or its share ended by itself -- the
    /// browser's own "Stop sharing" button).
    pub fn unpublish(&self, id: u64) {
        let mut st = self.lock();
        let Some(p) = st.publishers.remove(&id) else {
            return;
        };
        info!(socket = id, person = %p.person, stream = p.id, "stopped sharing");
        stop_watching(&mut st, p.id);
        broadcast_streams(&mut st);
        wants(&mut st);
    }

    /// This tab opened one desktop full size, or closed what it had.
    /// Nobody else's screen changes.
    pub fn watch(&self, id: u64, stream: Option<u16>) {
        let mut st = self.lock();
        let Some(c) = st.clients.get_mut(&id) else {
            return;
        };
        if c.watching == stream {
            return;
        }
        if let Some(old) = c.watching.take() {
            c.needs_key.remove(&(old, FULL));
        }
        // A stream that has gone away between the click and here.
        let live = stream.filter(|s| st.publishers.values().any(|p| p.id == *s));
        if let Some(s) = live {
            let c = st.clients.get_mut(&id).expect("just had it");
            c.watching = Some(s);
            c.needs_key.insert((s, FULL));
            debug!(socket = id, stream = s, "watching full size");
        }
        wants(&mut st);
        if let Some(s) = live {
            let socket = st
                .publishers
                .iter()
                .find(|(_, p)| p.id == s)
                .map(|(socket, _)| *socket);
            if let Some(socket) = socket {
                ask_key(&mut st, socket, FULL);
            }
        }
    }

    /// One encoded frame from a publisher, header and all, forwarded to
    /// whoever should have it.
    pub fn frame(&self, from: u64, mut msg: Vec<u8>) {
        if msg.len() <= HEADER || msg.len() > FRAME_MAX {
            debug!(socket = from, len = msg.len(), "bad screen frame");
            return;
        }
        let kind = msg[0];
        let key = msg[1] & KEY != 0;
        if kind != THUMB && kind != FULL {
            debug!(socket = from, kind, "unknown screen frame kind");
            return;
        }
        let mut st = self.lock();
        let Some(p) = st.publishers.get_mut(&from) else {
            // Frames before `publish`, or after `unpublish`.
            return;
        };
        let stream = p.id;
        if key {
            p.pending.remove(&kind);
        }
        msg[2..4].copy_from_slice(&stream.to_le_bytes());
        let mut wanting = false;
        let mut hopeless = Vec::new();
        for (id, c) in st.clients.iter_mut() {
            // The tab doing the sharing draws its own picture; it has no
            // use for a round trip of it.
            if *id == from {
                continue;
            }
            if kind == FULL && c.watching != Some(stream) {
                continue;
            }
            if !key && c.needs_key.contains(&(stream, kind)) {
                wanting = true;
                continue;
            }
            match c.frames.try_send(msg.clone()) {
                Ok(()) => {
                    if key {
                        c.needs_key.remove(&(stream, kind));
                    }
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Behind: stop until a keyframe can start them over,
                    // rather than queueing a picture they can't catch up
                    // with.
                    if c.needs_key.insert((stream, kind)) {
                        debug!(
                            socket = *id,
                            stream, kind, "viewer behind; waiting for a keyframe"
                        );
                    }
                    wanting = true;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => hopeless.push(*id),
            }
        }
        for id in hopeless {
            forget(&mut st, id);
        }
        // Asked for after the fan-out, and only while somebody is
        // actually waiting for one: `pending` keeps it to one request.
        if wanting && !key {
            ask_key(&mut st, from, kind);
        }
    }
}

fn streams_event(st: &State) -> Value {
    let mut streams: Vec<Value> = st
        .publishers
        .values()
        .map(|p| {
            json!({
                "id": p.id,
                "person": p.person,
                "codec": p.codec,
                "thumb": p.thumb,
                "full": p.full,
            })
        })
        .collect();
    streams.sort_by_key(|s| s["id"].as_u64().unwrap_or(0));
    json!({ "type": "streams", "streams": streams })
}

fn broadcast_streams(st: &mut State) {
    let ev = streams_event(st);
    let ids: Vec<u64> = st.clients.keys().copied().collect();
    for id in ids {
        send_ctl(st, id, &ev);
    }
}

/// Tell each publisher whether anyone has its full size open, so it can
/// stop encoding what nobody is looking at.
fn wants(st: &mut State) {
    let watched: HashSet<u16> = st.clients.values().filter_map(|c| c.watching).collect();
    let changed: Vec<(u64, bool)> = st
        .publishers
        .iter_mut()
        .filter_map(|(socket, p)| {
            let want = watched.contains(&p.id);
            (want != p.wanted).then(|| {
                p.wanted = want;
                (*socket, want)
            })
        })
        .collect();
    for (socket, want) in changed {
        send_ctl(st, socket, &json!({ "type": "want", "full": want }));
    }
}

/// Nobody can be watching a stream that has stopped.
fn stop_watching(st: &mut State, stream: u16) {
    for c in st.clients.values_mut() {
        if c.watching == Some(stream) {
            c.watching = None;
        }
        c.needs_key.retain(|(s, _)| *s != stream);
    }
}

/// Ask a publisher for a keyframe, once, until one arrives.
fn ask_key(st: &mut State, socket: u64, kind: u8) {
    let Some(p) = st.publishers.get_mut(&socket) else {
        return;
    };
    if kind == FULL && !p.wanted {
        return;
    }
    if !p.pending.insert(kind) {
        return;
    }
    if !send_ctl(st, socket, &json!({ "type": "keyframe", "kind": kind })) {
        // It never got the request: ask again on the next frame.
        if let Some(p) = st.publishers.get_mut(&socket) {
            p.pending.remove(&kind);
        }
    }
}

/// A control message to one tab. False if it didn't get it, which for a
/// tab this far behind means its socket is finished: it is dropped, and
/// the browser reconnects and asks for everything again.
fn send_ctl(st: &mut State, id: u64, v: &Value) -> bool {
    let Some(c) = st.clients.get(&id) else {
        return false;
    };
    match c.ctl.try_send(v.to_string()) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            warn!(
                socket = id,
                "screen control queue full; dropping the socket"
            );
            forget(st, id);
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            forget(st, id);
            false
        }
    }
}

/// Forget one socket, and whatever it was sharing.
fn forget(st: &mut State, id: u64) {
    st.clients.remove(&id);
    if let Some(p) = st.publishers.remove(&id) {
        info!(socket = id, person = %p.person, stream = p.id, "sharing ended with the socket");
        stop_watching(st, p.id);
        broadcast_streams(st);
    }
    wants(st);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALEX: u64 = 1;
    const RYAN: u64 = 2;
    const MIKE: u64 = 3;

    struct Tab {
        frames: mpsc::Receiver<Vec<u8>>,
        ctl: mpsc::Receiver<String>,
    }

    impl Tab {
        /// The (kind, stream, keyframe) of everything waiting.
        fn got(&mut self) -> Vec<(u8, u16, bool)> {
            let mut v = Vec::new();
            while let Ok(f) = self.frames.try_recv() {
                v.push((f[0], u16::from_le_bytes([f[2], f[3]]), f[1] & KEY != 0));
            }
            v
        }

        /// The keyframe requests waiting, by kind.
        fn asks(&mut self) -> Vec<u8> {
            self.control()
                .iter()
                .filter(|v| v["type"] == "keyframe")
                .map(|v| v["kind"].as_u64().unwrap() as u8)
                .collect()
        }

        fn control(&mut self) -> Vec<Value> {
            let mut v = Vec::new();
            while let Ok(t) = self.ctl.try_recv() {
                v.push(serde_json::from_str(&t).expect("control json"));
            }
            v
        }
    }

    fn tab(screen: &Screen, id: u64, person: &str) -> Tab {
        let (frames, ctl) = screen.add_client(id, person);
        Tab { frames, ctl }
    }

    fn frame(kind: u8, key: bool) -> Vec<u8> {
        let mut v = header(kind, key, 0, 0).to_vec();
        v.extend_from_slice(b"chunk");
        v
    }

    fn size() -> (Size, Size) {
        (Size { w: 320, h: 180 }, Size { w: 1920, h: 1080 })
    }

    fn share(screen: &Screen, id: u64) {
        let (thumb, full) = size();
        screen.publish(id, "vp8".into(), thumb, full);
    }

    #[test]
    fn a_thumbnail_reaches_everyone_but_the_sharer() {
        let screen = Screen::new();
        let mut alex = tab(&screen, ALEX, "Alex");
        let mut ryan = tab(&screen, RYAN, "Ryan");
        share(&screen, ALEX);
        screen.frame(ALEX, frame(THUMB, true));
        assert_eq!(ryan.got(), vec![(THUMB, 1, true)]);
        assert!(
            alex.got().is_empty(),
            "the tab sharing draws its own picture"
        );
    }

    #[test]
    fn the_full_size_only_goes_to_whoever_opened_it() {
        let screen = Screen::new();
        let _alex = tab(&screen, ALEX, "Alex");
        let mut ryan = tab(&screen, RYAN, "Ryan");
        let mut mike = tab(&screen, MIKE, "Mike");
        share(&screen, ALEX);
        screen.watch(RYAN, Some(1));
        screen.frame(ALEX, frame(FULL, true));
        screen.frame(ALEX, frame(FULL, false));
        assert_eq!(ryan.got(), vec![(FULL, 1, true), (FULL, 1, false)]);
        assert!(mike.got().is_empty(), "Mike never opened it");
    }

    #[test]
    fn a_publisher_is_told_whether_the_full_size_is_worth_encoding() {
        let screen = Screen::new();
        let mut alex = tab(&screen, ALEX, "Alex");
        let _ryan = tab(&screen, RYAN, "Ryan");
        share(&screen, ALEX);
        alex.control();
        screen.watch(RYAN, Some(1));
        let want = |c: Vec<Value>| {
            c.iter()
                .filter(|v| v["type"] == "want")
                .map(|v| v["full"].as_bool().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(want(alex.control()), vec![true]);
        screen.watch(RYAN, None);
        assert_eq!(want(alex.control()), vec![false]);
    }

    #[test]
    fn a_viewer_waits_for_a_keyframe_before_it_is_sent_anything() {
        let screen = Screen::new();
        let _alex = tab(&screen, ALEX, "Alex");
        share(&screen, ALEX);
        // Ryan arrives mid-stream, between keyframes.
        let mut ryan = tab(&screen, RYAN, "Ryan");
        screen.frame(ALEX, frame(THUMB, false));
        assert!(
            ryan.got().is_empty(),
            "a delta on its own would be a torn picture"
        );
        screen.frame(ALEX, frame(THUMB, true));
        screen.frame(ALEX, frame(THUMB, false));
        assert_eq!(ryan.got(), vec![(THUMB, 1, true), (THUMB, 1, false)]);
    }

    #[test]
    fn the_publisher_is_asked_for_a_keyframe_once_however_long_it_takes() {
        let screen = Screen::new();
        let mut alex = tab(&screen, ALEX, "Alex");
        share(&screen, ALEX);
        // Sharing asks for the first keyframe, and the encoder obliges.
        assert_eq!(alex.asks(), vec![THUMB]);
        screen.frame(ALEX, frame(THUMB, true));
        // Ryan arrives needing one of his own, and five frames go by
        // before Alex's encoder obliges again.
        let _ryan = tab(&screen, RYAN, "Ryan");
        for _ in 0..5 {
            screen.frame(ALEX, frame(THUMB, false));
        }
        let asks = alex.asks();
        assert_eq!(asks, vec![THUMB], "one request, not one per frame");
        // Once it has arrived, a later straggler asks again.
        screen.frame(ALEX, frame(THUMB, true));
        alex.control();
        let _mike = tab(&screen, MIKE, "Mike");
        assert_eq!(alex.asks(), vec![THUMB], "Mike needs one of his own");
    }

    #[test]
    fn a_viewer_who_falls_behind_is_put_back_to_the_next_keyframe() {
        let screen = Screen::new();
        let _alex = tab(&screen, ALEX, "Alex");
        let mut ryan = tab(&screen, RYAN, "Ryan");
        share(&screen, ALEX);
        screen.frame(ALEX, frame(THUMB, true));
        // Fill the queue without reading any of it.
        for _ in 0..VIEWER_QUEUE + 10 {
            screen.frame(ALEX, frame(THUMB, false));
        }
        let got = ryan.got();
        assert_eq!(
            got.len(),
            VIEWER_QUEUE,
            "dropped, not queued: {}",
            got.len()
        );
        // And nothing more until a keyframe, even with room again.
        screen.frame(ALEX, frame(THUMB, false));
        assert!(ryan.got().is_empty());
        screen.frame(ALEX, frame(THUMB, true));
        assert_eq!(ryan.got(), vec![(THUMB, 1, true)]);
    }

    #[test]
    fn resizing_keeps_the_stream_and_starts_the_decoders_again() {
        let screen = Screen::new();
        let _alex = tab(&screen, ALEX, "Alex");
        let mut ryan = tab(&screen, RYAN, "Ryan");
        share(&screen, ALEX);
        screen.frame(ALEX, frame(THUMB, true));
        ryan.got();
        screen.publish(
            ALEX,
            "vp8".into(),
            Size { w: 320, h: 200 },
            Size { w: 1280, h: 800 },
        );
        let streams = ryan
            .control()
            .into_iter()
            .rfind(|v| v["type"] == "streams")
            .expect("a streams event");
        assert_eq!(streams["streams"][0]["id"], 1, "the same stream");
        assert_eq!(streams["streams"][0]["full"]["w"], 1280);
        screen.frame(ALEX, frame(THUMB, false));
        assert!(ryan.got().is_empty(), "the old size is finished");
        screen.frame(ALEX, frame(THUMB, true));
        assert_eq!(ryan.got(), vec![(THUMB, 1, true)]);
    }

    #[test]
    fn leaving_takes_the_desktop_with_it() {
        let screen = Screen::new();
        let _alex = tab(&screen, ALEX, "Alex");
        let mut ryan = tab(&screen, RYAN, "Ryan");
        share(&screen, ALEX);
        screen.watch(RYAN, Some(1));
        ryan.control();
        screen.remove_client(ALEX);
        let last = ryan
            .control()
            .into_iter()
            .rfind(|v| v["type"] == "streams")
            .expect("a streams event");
        assert_eq!(last["streams"].as_array().unwrap().len(), 0);
        // Frames from a socket that has gone reach nobody.
        screen.frame(ALEX, frame(THUMB, true));
        assert!(ryan.got().is_empty());
    }

    #[test]
    fn watching_a_stream_that_has_gone_is_not_watching_anything() {
        let screen = Screen::new();
        let _alex = tab(&screen, ALEX, "Alex");
        let mut ryan = tab(&screen, RYAN, "Ryan");
        screen.watch(RYAN, Some(7));
        share(&screen, ALEX);
        screen.frame(ALEX, frame(FULL, true));
        assert!(ryan.got().is_empty(), "nothing was ever opened");
    }
}
