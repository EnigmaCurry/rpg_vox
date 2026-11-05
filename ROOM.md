# Voice room

`vox_room` is a voice chat in the browser for a few people and an AI.
Everyone logs in with their own invite link, joins the audio with a
headset, and hears everyone else and the AI (never themselves). Each
person's speech is transcribed live into one shared conversation that
updates on every screen at once, and the AI answers out loud through
Qwen3-TTS, its words appearing as they are spoken.

One server is several rooms, and everybody moves between them on the
one connection: the rooms of the house, and a room of your own where
you can talk to the AI with nobody listening. See
**[More than one room](#more-than-one-room)**.

It is full duplex: anyone can talk over the AI to interrupt it, with no
key to press. The conversation keeps only what the AI actually said
before it was cut off, ending with ` —`, and the AI is told where it was
interrupted. Each AI can also be set to **finish** instead, so that a
cough can't cost you the answer you asked for — see **Stops** under
**[In the room](#in-the-room)**.

It runs on macOS and Linux (no PipeWire: all the audio is the browsers').

There is an Android app, which is this same room in a WebView with a
foreground service holding it up, so a call goes on with the screen off:
see **[Voice room for Android](ROOM_ANDROID.md)**.

## Running it

```bash
just room-invite Ryan          # prints http://127.0.0.1:7340/join/…
just room-invite Alex
export VOX_SCRIBE_LLM_MODEL=nemotron-3-nano-4b   # your llama.cpp model
export VOX_ROOM_TTS_URL=https://qwen3-tts.example.com
just room --room Lounge --room Studio
```

Open your link, press **Join Audio** at the door, and talk. The speech-to-text models
are the ones scribe downloads (run `scribe` once if you haven't). Building
needs `pnpm` for the web app (`corepack enable` gives you one).

`just room --echo` runs without a model: the AI repeats what it heard.
Without `VOX_ROOM_TTS_URL` the voice is the local Kokoro model if scribe
has downloaded it (it does the first time it reads aloud with Kokoro),
else none: replies appear at reading pace, silently.

### Letting a friend in

Browsers only allow the microphone on HTTPS (or localhost), so put the
room behind a reverse proxy with TLS, and give it the public address so
invite links point there:

```bash
export VOX_ROOM_URL=https://room.example.com
just room-invite Alex          # https://room.example.com/join/…
```

Proxy everything, including WebSocket upgrades on `/room.ws` and
`/screen.ws`.

A link works **once**. Opening it spends nothing: it takes them to the
door, which says whose link it is and lets them set the name the room
will use. (That door doesn't say who is in yet — it has no cookie, and
so no socket to be told on.) **Join Audio** is what trades the link for a cookie, and from
then on that browser is who comes in — the link is dead, and anyone
opening it without the cookie is told it has expired.

`vox_room people` lists everyone, with the links nobody has used yet.
`vox_room uninvite NAME` revokes them: the link and every cookie traded
for one. The name there is the name on the invite, whatever they call
themselves in the room, and it stays theirs until they are uninvited.
`vox_room invite NAME` again, after they have joined, is a second link —
how somebody gets onto a second device, without disturbing the first.

### Who owns the room

One person **owns** the house, and they are made at the server:

```bash
vox_room invite-owner Ryan     # their link, and the only way to be one
```

The owner invites everybody else, hands out the rooms, and takes any of
it back, from inside the room itself — see [People](#people). There is
one, and the way to a different one is through the first:
`vox_room uninvite Ryan`, then `invite-owner` somebody else. It is a
command and not a button because it is the one thing nobody should be
able to do by clicking the wrong row, and because a house whose owner
can be changed from a browser is a house with a way in through its own
front door. A room with no owner still works; what it hasn't got is
anybody who can manage it from a browser.

`vox_room people` marks the owner. Everybody else is just somebody
invited: there are two kinds of person here and no more.

### Who may go where

- **A room of your own** is yours because it is yours. Nobody else can
  ask for it, the owner included — *Just you and Vox* is a promise the
  room makes on its face, and an owner with a key to it would make that
  a lie.
- **The lobby** — the first room named with `--room` — is everybody's.
  Calling in has to land somewhere, and a room that could be refused is
  not that somewhere.
- **Every other room** is given out one person at a time, by the owner.
  Until it is given, it is not on their list and not behind its address
  either: the server says the same thing about a room you may not enter
  as about a room that isn't there, because which of the two it is is
  not the asker's business.
- **The owner** has every room in the house without being given any.

A grant is kept against the room's id, so renaming a room keeps
everything: who may go to it, what was said in it, and its faders.

Upgrading a room that was running before any of this: everybody invited
keeps every room they could already walk into. Nothing is taken away by
the new rule; the owner can take it away afterwards.

## More than one room

One `vox_room` holds several rooms at once, and **the owner makes
them** — in the room itself, on the [People and
rooms](#people-and-rooms) page, while the server runs. A new house
starts with one room called **Lobby**; `--room` (or
`VOX_ROOM_ROOMS`) names the first rooms of a house that has none yet
and is ignored after that, because from the first start the house is
the database's. On top of those, **everybody has a room of their own**
— it isn't made by anybody, it appears the first time they walk into
it, and nobody else can ask for it.

A room is known by an **id of its own** that never changes, not by its
name: its conversations, its settings and who may go to it are all kept
against that, so the owner can call it something else without anything
following the word. Taking a room away takes it out of the house and
takes everyone's way into it; what was said in it is left in the
database rather than destroyed by a click.

Which room you are in is the heading of the page, top left, and every
room you may go to is behind it: press it and they drop down over the
strips, press one and the list shuts, you are there, and the heading
says so. **Your own room is first and stays first**, then the house's
in the order they were named: the one room that is always there and
always the same is the one to put where the eye lands, and a room that
moved about the list as people came and went would be a room you had
to look for. A house can hold a great many rooms, so the list scrolls
rather than running off the screen. A tab still *lands* in the first
shared room, because calling in is calling in to be with people.

The list opens **over** who is in the room rather than above them: the
strips are the one place to look for who is here and how loud they
are, and a list that pushed them down the screen every time you went
looking for another room would make you find them again afterwards.

**The audio never stops.** You called in once and you stay called in: what changes
is which room your voice goes into, whose conversation is on your
screen, and which mixer is feeding your headphones. Moving is a frame
of audio, not a reconnection.

Each room is a whole room, with everything the one room ever had: its
own conversation, its own faders, its own mode, its own games being
played, its own cards. Walk out of one and back into it and it is as
you left it. What is shared across them all is the invite list, the
saved games, the name you call yourself and the language you hear —
the things that are the person's rather than the room's.

A desktop you are sharing does **not** come with you. It was shared to
the room you were in, and carrying it into the next one would show a
room something nobody in it asked to see and give you no moment to
decide otherwise — so it stops as you leave, and sharing it where you
have arrived is something you ask for again. A dropped connection is
not leaving: the socket says which room it was sitting in when it comes
back, so a moment of bad wifi costs you neither your seat nor your
share.

A **room of your own** is where the AI is yours alone. Its roster is
you; its mixer shows you and the AI and nobody else; nothing said in it
reaches the house, and nothing said in the house reaches it. Because
there is nobody there to take the turn from, it answers every pause
rather than judging each one, which is what a room people share does. It is called *You and Vox*, after whatever you called
the AI, and it is the first thing in the list.

(A room called **Lobby** is a room. The *game* sense of "the lobby" —
being in no game, just the room's own conversation — is the room's
state, not a place you travel to, and the button that leaves a game
says **Leave the game** so that the two can't be read for each other.)

The faders for the room you are in hang off it in that same list, so
the mixer is part of the room rather than a panel beside it. **You are
in a room or you are not**: the list under it is who is there, and
somebody invited but elsewhere is simply not on it — they are on the
count beside the room they went to. There is no greyed-out half-presence
to read, because there is no such state.

Sharing a desktop follows the same rule as talking — it reaches the
room you are in and no other, and it goes with you when you leave.

### More than one AI in a room

Every room has the AI in it. `--bot NAME[:VOICE[:PROMPT_FILE]]` puts
another one on the server — repeat it, or comma-separate
`VOX_ROOM_BOTS`. Each has its own voice, its own prompt, and its own
name to be called by; the first is the one `--name`, `--voice` and
`--prompt` describe.

**Which rooms each of them is in is the owner's to say**, on the
[People and rooms](#in-the-room) page, with a tick per room exactly as
for people. The first AI is in every room and its ticks are fixed: it
is what a room *has*, the way a room has a lobby, and a room of your
own is named after it. Every other one is somewhere because the owner
put it there. They start in the rooms people share and in nobody's own
— a room of your own is yours, and a second AI arriving in it
uninvited is the thing this is for — and the house decides that once
per AI, so taking one out of the lobby stays done across a restart.

A tick reaches the room at once, wherever anybody is sitting: a fader
appears or goes, and an AI taken out of a room it was speaking in
stops where it is. Nothing is torn down — it keeps its thread, its
settings and everything it has said there, and putting it back brings
it back to all of it.

```
vox_room --bot Sage:serena --bot Ada:aiden:/etc/vox_room/ada.md
```

What each of them is is its own:

- **Its own name.** Saying "Sage" gives Sage the turn and nobody else.
  Nobody in the room may call themselves after any of them.
- **Its own fader.** One strip each, under the room, so turning one
  down is turning *that one* down.
- **Its own turn.** How each takes a pause is per bot and per room —
  Sage answers every pause in your own room, judges them in the Lobby,
  and says nothing at all while a show it is in is on. Nobody sets it
  from a control any more: whether an AI may answer *you* is
  [your own switch](#when-an-ai-may-answer-you), and the rest is the
  room's own doing.
- **Its own Stops.** Whether a voice talking over it cuts it off or it
  finishes what it was saying. Per bot and per room as well, and never
  a game's to take.
- **Its own prompt**, with `{name}` filled in as itself, so each is
  told who it is rather than told it is the first of them.
- **Its own side of the conversation.** Each AI is shown what the
  others said the way it is shown what a person said — a line of the
  transcript with the name on it — and its own words as its own. It is
  told which of those names are AI voices rather than people, so "Cora
  said" reads as what it is. A bot that was shown another's reply as
  its own would answer for it, or argue with itself.

The first AI can also put **[puppets](#puppets)** in the room:
characters with a name, a face and a voice and no brain at all, which it
speaks through. A puppet is how a room gets a scripted scene with
timing in it — an interjection landing on the half-second, one voice
cutting another off — which two AIs taking turns with a model each
cannot do.

**What each of them may do is not the same.** The first AI is the one
the room was built around and the one that runs it: the cards, the
games, standing down, the web. Every other bot is a voice in it, and a
voice may do two things — `list_people`, to see who is here, and
`look_up`, to search the web where anybody can (the lobby, with
`--search`). No cards, no games, nothing that changes the room. It
keeps `stay_quiet` and `wait_until_named`, because those are not powers
but how a turn is given back: without them a bot can't be put on "when
it fits" at all, and can't get out of a back-and-forth it was pulled
into. A tool it was never offered is refused if the model asks for it
anyway, and its prompt never mentions the ones it hasn't got — a note
about dealing cards, to a voice that cannot deal one, is an instruction
to try.

**They can put something to each other, and the people keep that
going.** A reply never starts another one by itself — the room hands
the turn over when somebody has said something — but **a name is
somebody saying something, whichever mouth it came out of**. When a
bot finishes a reply that named another voice in the room, that voice
is given the turn, exactly as when a person says a name. "Say hi to
Cora" gets a hello that Cora answers. `hand_to` does the same thing
deliberately, for when a bot wants the next turn to be somebody
else's without naming them aloud. That is what makes a conversation
of three rather than two people taking turns with one AI.

A name in passing ("I'll ask Cora later") hands over too, and that is
on purpose rather than tolerated: the turn it hands over is one the
other bot can give straight back, and a turn given back leaves
nothing at all — no bubble, no "thinking", nothing on anyone's screen.
The cost of reading a mention as an address is one quiet turn; the
cost of not reading it that way is the thing you actually asked for
not happening.

It is bounded by `--ai-turns` (default 2): replies the AIs may give
each other with nobody human saying anything in between. **The count
goes back to nothing the moment you speak or type**, so as long as you
are in the conversation they can keep going round; stop, and so do
they, within a turn or two. `--ai-turns 0` is AIs that only ever
answer people.

A bot is *told* when it is on the last of them, and isn't offered
`hand_to` at all then — it is asked to finish the thought and put it
back to the people. A bot that found out by being refused would
already have written a reply for a conversation about to stop dead.

**Who answers a pause.** Every bot that could answer is given the turn
at once, and **the first to actually say something keeps it** — the
rest are told to stop. Asking them one at a time would put a round
trip of silence in the room for every bot that had nothing to say, and
letting them all finish would be two voices over each other. A bot
that is cut off this way leaves nothing behind: no bubble, nothing in
the conversation, nothing for the model to read later. A bot already
speaking is never cut off by another starting — it is the first word
that claims the pause, and it has already had it.

So "when it fits" is the mode that makes a room of several AIs work:
each judges the pause for itself, and whichever has something to say
gets there first. Put them all on "every pause" and whichever thread
wins the race answers, which is a coin toss rather than a
conversation.

### Where each room lives

Every room has an address, and the bar follows you from one to another.
Back and forward move between rooms, because that is what the bar
changing means, and a link is a link: paste one to somebody and it
opens that room for them.

| | |
|---|---|
| `/` | your own room — per person, so whoever opens it gets theirs |
| `/lobby` | the first room named with `--room`, where a tab lands |
| `/room/<id>` | every other room |

The id is not the room's name. A name is a thing people retype and
change their minds about, and a link that changed with it would be a
link somebody had already sent on; so each room is given an unguessable
id the first time it opens and keeps it. An address that names no room
— a stale link, a typo — quietly becomes the address of the room you
ended up in rather than sitting there broken.

An invite link is served the app and nothing else — no redirect, no
cookie, nothing written down. **The room hands a browser nothing until
Join Audio.** The token stays in the address bar, where whoever
followed the link put it; the page keeps it in memory and sends it with
each of the three requests the door makes, and pressing **Join Audio**
is what spends it for the one cookie this browser is ever given. So a
link you opened and thought better of leaves no trace: close the tab
and there is nothing to find, and a browser with no cookie and no link
in the bar is one the room knows nothing about — which is what being
logged out is.

Once the link is spent, the address bar is written over with the lobby's
address, which is where you land: coming in on an invite is coming in to
be with people.

### What a room has said before

**New conversation** closes the one the room is having and starts a
blank one. The old one isn't thrown away — it becomes part of that
room's history, and **History**, at the head of the conversation, is
the list of them: what each is called, how much was said, and the
first thing anybody said in it, newest first with the live one marked
**now**.

**Calling one something.** The name at the head of the conversation is
a box: type over it and that is what the room calls this conversation,
for everyone in it and in the history from then on. It names whichever
one you have open, the old ones included — read-only means nothing can
be *said* into it, and a name is not something said; being able to name
one you forgot to name is most of what naming is for. Left alone it is
**when it was** — the time of the first thing said in it, or the day it
was started if nothing has been. Emptying the box gives it back to the
clock rather than leaving it blank. (A date is a thing each browser
spells in its own language and timezone, so the room keeps the name and
the numbers and says nothing about how they read.)

A conversation **nobody said anything in** is not one the room had, and
isn't on the list: pressing **New conversation** twice leaves nothing
behind the second time. The one the room is having now is always
there, empty or not — that is where you are, not an archive of
anywhere.

Open one and you read it exactly as it was, with the translations and
the cards that were in it. It is **read-only**, and not by a rule: the
room has moved on from it, so there is nowhere for anything you said
to go, and the box you would have typed into isn't there. **Back to
the conversation** returns you to the live one.

**Reading is yours alone.** Nobody else's screen moves when you open
an old conversation, which is the only way it could work: a room where
one person looking something up pulled everyone else out of the
conversation they were having would be a room nobody dared look
anything up in. The one thing in there that *is* the room's is the
**×** beside each: deleting takes that conversation, everything said
in it and every card dealt in it away from everybody, for good, and
asks first. The live one has no **×** — it is the room, not an archive
of it.

Each room's history is its own, and a conversation can only be read
from the room that had it.

### A game is something a room does

A game is played **in a room**, by the people in that room, and while
it runs the game is what that room is: its conversation is the game's,
and the cards dealt in it are the game's. The room keeps its name in
the heading with the game beside it, and the list of rooms says which
room has a game on, so a room worth walking into looks like one from
outside it.

**Leaving the game puts the room back where it was.** What the room was
talking about is put aside, not thrown away: **Leave the game** picks
that conversation up again — every word of it, and every card anyone
was holding — for everyone in the room at once. A game is an
interruption, and the room goes on afterwards.

The game's own conversation is kept too, like everything else here; it
is just not what entering the game again gives you, which is the game
from the top. **Restart game** is not the same as finishing with one:
the room still has a conversation to come back to afterwards. And a
room left in the middle of a game is still in it after a restart of the
server, and still knows what it interrupted.

Rooms cost a little: each is its own mixer, its own agent and its own
voices. What they share is everything expensive — the speech-to-text
models, the translator, the endpoint the AI is asked on — so a handful
of rooms is a handful of mostly-sleeping threads, and an empty one
costs nothing at all.

A room's conversation, its mode and its faders are kept in the database
under its name, so **renaming a room starts it fresh** and the old one's
conversation sits there waiting to be found again if the name comes
back. A database from before any of this had one room in it, and that
room's things become the first `--room`'s on the way up.

### In Docker

`docker/` has a compose deployment behind Traefik, with the labels
interpolated from `docker/.env` and the speech-to-text models downloaded
into a volume on first start:

```bash
cd docker
just config                    # asks about each setting, writes .env
just install
just room-invite Alex
```

See ROOM_DOCKER.md.

## In the room

- **The door**: the room is voice only, so a tab that hasn't joined the
  audio is **at the door** and nowhere else — a page of its own, with no
  room behind it. Nothing of the room is there to see and nothing of it
  is open: the sockets are the room's, and until you come in you are not
  on a roster, not sent a conversation, and not counted as being
  anywhere. Everyone in the room is in it by ear, and a conversation you
  can read but not hear is not being in it.

  Pick your microphone and speakers there (both are remembered for next
  time), and push to talk, before you come in. **Leave audio** puts you
  back at the door and takes the sockets with it.

  **Agreeing to be heard.** Coming in means your microphone goes live to
  everyone else in the room, what you say is transcribed on the server
  and written into a conversation the room keeps, and that writing is
  sent on to the AI and to the translation and voice services the
  operator has configured — all outside the room. The card says so and
  asks you to agree, and **Join Audio** does nothing until you do. The
  answer is kept in the browser, so it is asked once; unticking it takes
  it back and shuts the door again. The first time, on an invite link, it says *Hi,
  Mike* and offers **your name in the room**: that is the name everyone
  sees, the one beside everything you say, and the one the AI deals your
  cards to. It is yours to change here and from **Settings → Your name**
  afterwards, and it is the only name you can change. (The name on the
  invite doesn't move with it: whoever runs the room chose that one, it
  is the key `vox_room uninvite` names, and a room of your own is yours
  by it.) **Log out**, at the foot of the
  card, hands this browser's cookie back and forgets it — only this one,
  so another device stays in. The link it came in on is spent, so coming
  back takes a new one. Choosing a speaker needs `AudioContext.setSinkId`,
  which today means Chrome; elsewhere the room follows the system default
  and the list isn't offered.
- **There is no top bar.** What a room is called belongs to the room
  column, what a conversation is called and what is done to it belong
  over the conversation, and what you reach for while talking belongs
  in the footer — so each of those is now where it is about, and the
  row they used to share is a row of the conversation instead.
- **The room**, top left: which one you are in, as the heading of the
  page, and every room you may go to behind it. Pressing one takes you
  there without the audio stopping. See
  [More than one room](#more-than-one-room). Underneath it, always in
  the same place however the list above it is being used, is everybody
  who is in that room — their faders, their meters, their mutes, and
  the AI's — so the mixer is part of the room it mixes rather than a
  panel of its own. Who is in
  it, and nobody else: somebody invited but in another room is on that
  room's count, not greyed out on this one. The rooms you are not in
  stay one line, because a list that has to hold a great many rooms has
  to stay a list you can read.
- **The areas**: the rooms on the left, then what is yours alone, then
  the room's conversation. **Just for you** is only there while you are
  holding something: with no cards it is not an empty column but no
  column at all, and the conversation has the half it was taking. On a
  narrow screen one area shows at a time, with a switch above it —
  **Conversation**, **Just for you** (with a count, and only while you
  hold a card), **People**. The room's heading sits above the tabs
  rather than inside one, because which room you are in is true of
  whichever area you are looking at.
- **The bar over the conversation**: what this one is called, and
  everything done to it. **History** opens the ones this room has had
  before, to read; **New conversation** starts the next one; and while
  a game is on, its name and **Leave the game**. See [What a room has
  said before](#what-a-room-has-said-before).
- **BOTS**, in the footer: whether the AIs may answer **you**. See
  [When an AI may answer you](#when-an-ai-may-answer-you) — it is one
  switch per person rather than one control per bot, and it is the one
  thing here that is yours alone.
- **How a bot takes a pause** is the room's own, and there is nothing to
  press for it. What it can be:
  - **Every pause** — it answers whenever the room goes quiet, no
    judgment involved. What a room of your own does, since there is
    nobody to take the turn from, and what a game that deals the turns
    asks for.
  - **When it fits** — at every pause the AI is *given the turn* and
    decides for itself whether the moment is its own. What a room
    people share does. It is shown the
    conversation, **who is in the room**, and whether it is already
    mid-conversation, because a pause means different things in different
    rooms: with one person there is nobody else they could be talking to,
    while three people talking among themselves own the gaps between
    them. Take the turn when the last thing said was meant for it; let it
    pass otherwise. If it passes, it calls `stay_quiet` and **nothing
    happens at all**: no bubble, no "thinking", nothing on anyone's screen. It can
    also pass with `ask_again_in_seconds`, and the room shows it the
    conversation again if the quiet actually lasts that long — how it
    waits for a real silence instead of the 1.2 s gap it was shown. Only
    once per silence, so a quiet room can't loop.
  - **When named** — it says nothing until someone says its name. Where
    a bot puts *itself* when it decides it is not wanted for a while
    (its `wait_until_named` tool — "thanks", "goodbye", or you simply
    turning to talk to somebody else), and what a game can ask for.
  - **Never** — it says nothing at all: not when it is named, and not
    in a game that wants every pause answered. A show puts its director
    and its cast here for as long as the curtain is up, and it stops
    one mid-sentence rather than waiting for the sentence to end.

  The strip under the bot says which of these it is doing whenever it
  is doing something worth saying — *In the show*, *Waiting to be
  named*, *In a back-and-forth*, *Answering every pause* — with why in
  the tooltip. None of it is written down: a room comes back up after a
  restart taking its pauses the way the room is configured to, because
  a voice that went quiet about this afternoon's conversation should
  not come back quiet about tomorrow's with nothing on any screen to
  say why.
- **The back-and-forth**: to a bot waiting to be named, saying its name
  doesn't buy you one answer, it starts a conversation. From then on it
  keeps the turn at every pause without being named again — still
  judging each pause rather than answering every one — until the room
  has been idle for 30 seconds (`--engaged-for-ms`), when it goes back
  to waiting for its name.
- **Interrupting**: start talking while it speaks. It drops in volume at
  once and stops for good once your speech is confirmed (300 ms of voice
  or the first recognised word), so a cough only dips it. **Stop** does
  the same with a click.
- **Stops: when interrupted / when finished**, under that bot's strip:
  the one thing about an AI that is still set per AI, and the one a
  game never takes — a game says when a bot answers, never how it is
  stopped. Its own setting in that room.
  - **When interrupted** — what is written just above. The default, and
    what a conversation wants: talking over it is how you take the turn
    back.
  - **When finished** — it says the whole of its reply. A voice still
    ducks it, so you can be heard over it, but nothing cuts it off: ask
    it a question and a cough, a false start or a word to somebody else
    can't cost you the answer. **Stop** still stops it — only the voice
    is disarmed, never the button.

    What you said over it is not thrown away and not treated as a
    question either. It is transcribed and on screen as always, it
    waits where it is while the reply plays out (the turn isn't over
    until the AI has finished), and then it is handed over as a pause
    for the AI to *judge* — the same judgment "when it fits" makes,
    even when the bot is on "every pause", and it is told the words
    came in over its own voice. So a throat-clear costs nothing, and a
    real follow-up asked while it was still talking gets its answer one
    turn later.
- **Typing instead of saying it**: there is a box along the foot of
  the room, beside the button you talk with — saying a thing and typing
  it are the same act done two ways. **Enter** sends;
  **Shift+Enter** starts a line, and the box grows to hold what is in
  it, up to half the window, after which it scrolls. What you send
  through it appears beside everything said,
  marked **typed** — the mark is the point, because a typed line has no
  recognition error in it. It is for everything speech-to-text is bound
  to spoil: a URL, a path, a command, an unusual name. The AI reads it
  like anything else, and naming the AI in one gets its attention the
  same way.

  **Typing never interrupts.** Starting to type doesn't dip the AI and
  sending doesn't cut it off, however much you send: only a voice does
  that. A message joins the conversation at the next pause, with the
  same beat as speech in which to be joined by more.

  Messages are capped at 2000 characters, and keep the line breaks you
  gave them — a list is a list. **What the AI is shown is one line per
  turn**, which is where that used to be enforced and is the only place
  it ever mattered: the model reads the conversation as `Name: what
  they said` lines, so a break inside one of them would read as the
  start of somebody else's turn. People read the breaks; the model
  reads the line. Runs of spaces become one, at most one blank line in
  a row survives, and Unicode's control codes, direction overrides and
  zero-width characters are still spaces. Everything else survives
  exactly as typed, because the punctuation a careless filter eats is
  what a URL is made of.
- **Dropping a picture in**: drag one onto the room and let go of it
  anywhere, or paste one with **Ctrl+V**.
  It becomes a card in the conversation with your name on it
  and the next number, which is the number the AI takes for
  `set_avatar` — so *"make that my face"* is the whole of what you
  have to say after dropping a photograph in. Only pictures; anything
  else is declined where you dropped it. Ask for it changed — a hat, a
  colour — and the AI gives it to the picture server first, which
  redraws it in one of its looks; yours stays where it is. See
  [Dropping a picture in](#dropping-a-picture-in) and [Changing a
  picture you dropped in](#changing-a-picture-you-dropped-in).
- **More than one language**: start the room with
  `--language-also Russian` and the door offers a **Language** beside
  the devices. Everyone hears the people in the room as they are — their
  own voices, as always — and the AI in the language they picked: it
  writes its reply once, in the room's own `--language`, and a separate
  voice per language translates and speaks it. So two people can be in
  the same conversation, each hearing the AI in their own language, with
  one shared transcript. It needs `--tts qwen3`; Kokoro only speaks
  English.

  The door's **Language** box always has a language in it, never a
  blank: whatever the system is set to the first time, if the room
  speaks it, and the room's own otherwise. That is a guess, and it is
  made once — it is written down in the browser, so it can never
  afterwards overrule something you picked.

  Your choice follows your invite rather than your browser, so it is
  there on your phone too, and you can change it later from the door.
  A language nobody has picked costs nothing: the AI is only translated
  while somebody is listening in it.

  A typed message is translated too, and both are kept, so coming back
  to a conversation shows what everyone was shown at the time. The link
  under it is how you get at the characters that were actually typed —
  which is what you want when the message is a URL or a command, since
  a translation is the one thing you must not copy those out of.

  Three things are worth knowing. **You hear your own language or
  nothing** — never a word of anybody else's. A translation has to be
  written before it can be spoken, so it begins about three seconds
  after the original does, and in those seconds you hear silence rather
  than the opening of the reply in a language you didn't ask for.
  (Measured on a real room: the AI's first English audio at 3.6 s, the
  same reply's first Russian audio at 6.3 s.)

  Second, **if you are listening in a translation you cannot talk the AI
  down**: starting to speak neither dips it nor cuts it, because you
  would be stopping it in the middle of something you haven't heard yet.
  **Stop** still works for you, since a click says where to cut without
  guessing.

  Third, **you read what you heard**. The AI's replies and anyone's
  typed messages appear in your language, with a **Show English** link
  under them for the words they were written in — and a **Show Russian**
  link to come back. One language at a time, with the other a click
  away: enough to catch a translation that went wrong, without two
  copies of every line to read past. Which one you are looking at is
  yours alone and changes nobody else's screen.

  The words arrive at the pace of the voice reading them *to you*. A
  translation is a different number of syllables from the words it
  translates, so each language fills in at its own rate, and a line
  translated in a tenth of a second still takes as long to read as it
  takes to say. The AI's turn is over when the last language has been
  heard out, not when the first has — otherwise ending it would take
  the rest of the reply away from whoever was still listening to it.
  Which also means a cut lands where each voice was: the dash that
  marks it is in a different place in each language.

  So that nobody has to guess at that, a reply still being spoken
  carries **a small bar per language** under it, saying how far that
  language has been heard and how much of it is left:

  ```
  ENGLISH ▬▬▬▬▬ heard    RUSSIAN ▬▬▬░░ 2s
  ```

  Everyone sees the same row, whichever language they are listening in,
  and it goes when every language has heard the reply out — which, for
  the AI, is the same moment the turn ends and it will take another. An English
  speaker can see that the Russian still has a sentence to go before
  saying anything; a Russian listener can see that the room is waiting
  for them rather than that something has stalled. A language whose
  translation hasn't come back yet says `translating…` rather than
  pretending to be at nought seconds.

  Underneath, the room's own language is still the record: it is what
  it is stored as having said, however many languages it was heard in.

  **People are interpreted too.** What anyone says out loud is said
  again, by a separate voice, to everybody in the room who doesn't speak
  it — the way a booth at a conference does it, and with the same
  ducking: the voices being interpreted drop to a fifth under their
  interpreter, and come back when it stops. Only *those* voices duck.
  Somebody speaking your own language stays exactly where they were,
  because you can follow them and nothing is being said over them.

  It starts when an utterance has been decoded whole — pass 2, so a
  phrase behind the speaker rather than word by word, since the words of
  a streaming guess are still changing. The text arrives with it, so
  everyone reads the room in their own language and keeps the **Show
  English** link to what was actually said. Pass 3 rewrites the
  transcript afterwards and is deliberately not spoken again: by then
  the words have been heard, and nobody can unhear them.

  An interpretation waits while the AI is speaking in that same
  language. Both of them are in your language and two voices at once is
  neither; what is waiting keeps its place and follows on.

  **It is never more than a few seconds behind, and it never skips
  anything quietly.** An interpreter that has more than eight seconds
  of speech still to say stops speaking and only writes: another voice
  on the end of that queue would be heard later still, and by then
  nobody wants it. The same when it is turned down, when a translation
  fails, and when even the translating cannot keep up — the words go on
  screen either way, as they were said if they could not be put into
  your language at all. A reader who is given nothing has no way of
  telling that anything was skipped, which is the one outcome worse
  than reading a line in a language you didn't ask for. `the
  interpreter is behind` and `cannot keep up` in the log are what it
  looks like from outside.

  What makes it fall behind is not usually the translating — a Marian
  model is fifty milliseconds a line and even the reply model is about
  two seconds, both faster than the speech they stand in for. It is
  having nowhere to put the audio: a long AI turn holds every
  interpretation back until it ends, and a room where everyone talks at
  once makes more speech than there is time to say it in.

  The same bar appears under a line somebody is still being heard
  saying, so the speaker can see when the room has caught up with them.
  It costs a translation and a voice per utterance per language, which
  is the same arithmetic as the AI's replies and the same two
  requirements: a translator and `--tts qwen3`.

  **The interpreter is yours.** It is the one channel in the mixer that
  isn't the room's: you hear the voice speaking *your* language and no
  other, so how loud it is is a thing you do to your own headphones,
  and turning it down does nothing to anybody else's. It appears in the
  list under the room **only while it has work** — somebody in the room
  speaking a language you don't read. Everyone sharing one language,
  and there is no strip; a room of your own can never have one, since
  the AI is already answering in your language and there is nobody else
  in there to interpret.

  The duck follows your fader: at unity the voices being interpreted
  drop the usual 14 dB *for you*, and every unit you add takes another
  14 dB off them — a louder interpreter is a quieter floor, which is
  the only way louder is any use. All the way down (or muted) it is
  **off for you**: nothing is spoken over you and nothing ducks, and if
  nobody listening in that language wants it, no voice is paid for at
  all. What people say is still put into the other languages and
  written down either way, so everyone keeps reading the room in their
  own — turning the voice off is not the same as turning the room
  monolingual. For that, `--interpret false`: no interpreters at all,
  and a line spoken in a language you don't read is shown as it was
  said.

  What the AI is shown is the room's own language throughout: where an
  interpretation into it exists, that is the line the model gets, so a
  conversation the room heard in two languages reads to it as one. It is
  not waited for — a turn is not held up for a translation that hasn't
  come back — and the model is told who is listening in what either way.

  **Who translates.** By default the same model that writes the replies
  (`--translate openai`). `--translate local` asks Marian models on this
  machine instead — `just download-translate-model-ru` fetches the
  Russian pair, about 80 MB each — and then nothing anyone says leaves
  the room. Measured side by side on one machine
  (`cargo test -p vox_transcribe --features ct2,llm --lib -- --ignored
  --nocapture compare`):

  | | `openai` | `local` |
  |---|---|---|
  | A line | ~1900 ms | **~110 ms** |
  | Network | required | **none** |
  | Prose | best | very close |
  | A URL | kept | **kept** |
  | A command, bare | mostly kept | **mangled** |
  | A command in backticks | kept | **kept** |
  | Earlier lines for context | yes | no |

  Being seventeen times faster matters more than it sounds: it is most
  of the three-second wait above, so a translated listener hears the AI
  almost when everyone else does.

  What it costs is that a Marian model cannot be *told* anything — no
  "copy a URL through as it stands" — so left alone it renders
  `--language-also Russian` as the Russian for "also Russian language",
  and `Ctrl-C` as "2-С". So **put anything that has to arrive exactly in
  backticks**: `` `like this` ``, or a ```` ``` ```` fence. Those are
  lifted out of the line before the model sees it and put back after,
  which makes them safe whichever translator is in use. Backticks are
  worth the habit either way; with `--translate local` they are the only
  thing that works.

  Your words are written down in whatever you speak: Parakeet works out
  the language from the audio, and needs no telling. The text that
  appears *while* you are still talking is the exception — that pass
  needs a model per language (`just download-streaming-stt-model-ru`,
  or `scribe download-models --russian`), and without one your words
  simply appear a phrase later, once the sentence is decoded.

  That pass follows the language you chose at the door, so **if you
  choose one language and speak another, the text that appears as you
  talk will be an approximation of your words in the wrong alphabet.**
  It is corrected a moment later, when the sentence is decoded by the
  model that does work the language out. Nothing is lost, but it looks
  odd while it happens; choose the language you are actually going to
  speak.
- **Sharing a desktop**: **📺️**, over the people in the left column,
  offers the browser's own picker of a window or a screen. Everyone in the room
  gets a thumbnail of it under that person's name in the mixer, live;
  click one and it opens full size where the conversation was, with the
  mixer (and everyone else's thumbnails) still beside it. **That is
  yours alone** — opening a desktop, or closing it, changes nothing on
  anyone else's screen, and nothing about it is remembered. Several
  people can share at once; each gets a thumbnail. Pressing **📺️**
  again, the browser's own stop button, leaving the audio and walking
  into another room all end it. No sound is
  taken from what is shared: the room's audio is the mixer's, and a
  shared tab's would come back as another voice to transcribe.
- **The footer** is the two ways of saying something, side by side,
  and nothing else. **Mute** (or **Hold to talk**) is one width
  whatever it says, so the button under your hand is the same button it
  was a moment ago and nothing beside it moves. Beside it, stacked and
  half its height each so the pair is exactly as tall as it is:
  **PTT**, which is what turns that button into one you hold, and
  **BOTS**, which is whether the AIs may answer you. Both are yours
  alone, both light up when they are on, and both have a nuance on the
  settings page — what the *off* position means. Then the box you type
  in, taking the rest of the width. Anything the room has to tell you
  appears along it too.
- **What you do with the room** is three icons over the people in the
  left column, where the room they are about is: **📺️** share a
  desktop, **🚪** leave the audio, and the cog. They are not in the
  footer because the footer is for the button you aim at without
  looking, and these are not it.
- **Settings**, behind the cog: a page of its own with
  the things you set once and don't watch — **Your name**, **App
  volume**, which microphone the browser gave the room, what
  **holding the talk button** does to the room's own sound (see the
  mixer below), and what **turning the AI off** means for you (see
  [When an AI may answer you](#when-an-ai-may-answer-you)). Changing
  your name changes it everywhere, the conversation
  included: the room calls you one thing, not a different thing per
  paragraph. The room
  goes on running behind it: you are still in the audio, and the space
  bar still mutes or talks from there. **Done** or Escape comes back.
- **People and rooms**, on the settings page and only for the owner: a
  page of its own with the house on it. Make a room, call it something
  else, or take it away; invite somebody and copy their link, give and
  take away rooms with a tick, make somebody a fresh link when theirs
  goes astray, or remove them — link, browsers and rooms together. The
  lobby cannot be taken away and the owner's own row has no Remove on
  it: see [Who owns the room](#who-owns-the-room). Nobody but the owner
  is shown any of this, and nobody but the owner is answered when they
  ask the server for it either.

  **The AI voices** are a second table under the people, asked the same
  question: which rooms each of them is in, a tick per room, with the
  lobby among them (a room can be one no AI answers in) and one column
  for **rooms of their own**. The first AI is in all of them and its
  ticks are fixed, the way the owner's are in the table above. Which
  AIs the server has is `--bot`'s to say and not this page's: this is
  where they are, not whether they exist. See
  [More than one AI in a room](#more-than-one-ai-in-a-room).

  Taking a room away, or taking it away from one person, happens where
  they are sitting: anybody left in a room that has stopped being
  theirs is put back in the lobby and told so. A room nobody may enter
  is not a room to leave somebody in — it would be off every list, with
  no way out of it but the door.
- **Mixer**: a level and mute for each person and each AI, shared
  by the whole room, hanging off the room you are in in the list on the
  left. **App volume**, in the settings, is only yours. Space mutes
  your mic, or with **Push to talk** on, your mic is open only while you
  hold Space (or the talk button): everything said in one hold is one
  entry, and the AI waits for you to let go however long you pause.

  **An open mic is full duplex**: everybody hears everybody, all the
  time, and nothing of the room's sound is ever cut.

  **What holding the key does to the room's own sound is yours to
  settle**, in Settings → *Holding the talk button*, because the two
  rooms it is held in want opposite things of it.

  *You keep hearing the room* — full duplex, and the default — is for
  a headset or headphones: nothing is cut, so holding the key to
  interrupt lets you hear the AI stop.

  *The room goes quiet* is half duplex, and is for speakers with a
  microphone pointed at them: whatever the room is saying goes into
  that mic, is written down as theirs, and is read back as something
  they said — and echo cancellation is deliberately off here, so
  nothing else is keeping it out. A microphone cannot be made deaf to
  one sound in the room it is in, so the room stops making it: the
  sound is taken to nothing as the key goes down and brought back when
  the mic has actually closed, which is a moment after you let go,
  because that moment is still a microphone in front of the speakers.
  It is a fade of a few milliseconds and not a switch, since a hard cut
  is a click and a click is a sound the mic hears too.

  What the room said while you held is then missed rather than queued:
  a room that caught up afterwards would be a room talking over itself,
  a second behind, every time somebody let go. That is the cost of the
  half-duplex setting and the reason it is not the default — it buys
  nothing on headphones, where there is no sound in front of the mic to
  cut. The setting is a page away from the key and still lands on the
  hold you are in: the room goes on running behind that page.

  **Which key talks is yours to pick.** Space out of the box;
  **Settings → The talk key** binds another. You press the key rather
  than naming it, so there is nothing to spell wrong, and a key the
  browser never hands the room simply doesn't arrive — the button goes
  on waiting, which is the truth about that key. Three kinds do arrive
  and are still refused, with the reason said at the moment you press
  them: ones the browser keeps (`Ctrl+W` would close the tab and take
  the audio with it), ones the room has already spent (Escape is how
  you get the keyboard back; Enter and Tab are how a keyboard works the
  page at all), and a modifier on its own, which is held down whenever
  you use it with anything else.

  Combinations are allowed. Holding `Ctrl+Space` and letting go of Ctrl
  first still closes your microphone: the room matches the key *and*
  the modifiers going down, and the key alone coming up. A release that
  insisted on the modifiers would leave the mic open with nothing on
  the screen saying so, which in a room where everybody hears everybody
  is the worst thing this app could do. Losing the window closes it
  too, as it always did.

  Like the theme, the binding is this browser's and never reaches the
  server: it is a fact about a keyboard, not about a person.

  **The talk key is the microphone's wherever you left the focus.** On
  a button you have just clicked -- a room in the list, a mute,
  somebody's shared screen -- it still talks, and it does not press that
  button again while you hold it. The exceptions are where Space is
  something else: a text box, a dropdown, and a dialog that is asking
  you something. A focused button is still worked by Enter.

  **Escape gives the keyboard back.** A text box and a dropdown both
  keep the focus after you have finished with them, so without this the
  space bar stays theirs until you think to click the background —
  something to know rather than something to see. Escape lets go of
  whatever has it and changes nothing else: what you had typed is still
  in the box to come back to. It is only ever about the box, too, so a
  press meant for the name field does not also take the settings page
  out from under you; press it again for that. (A dropdown that is
  *open* is the one case where the first press does nothing visible —
  the browser keeps that one to close the list, and the next one
  reaches the room.) People who run a vim-keys extension already have
  this key; this is the same one, for everybody else.

  Mute, push to talk and the mic belong to the **tab**, not to you, so
  two of your own tabs can't argue over them: you are muted once every
  tab with its mic open says so, and a tab sitting at the door — or left
  open on another screen with push to talk on — has no say in what the
  one you are speaking into does.
- **New conversation** clears the screen and the AI's memory. Old
  conversations stay in the database.

### When an AI may answer you

**BOTS**, under **PTT** in the footer. On, which is how everybody
starts: the AIs in the room may answer what you say. Off: they stay out
of it.

**It is yours and nobody else's.** When an AI answered used to be the
AI's own setting — one *Answers* control per bot, shared by the whole
room and changeable by anybody in it — which made being answered a
thing the room decided about you. Two people at one table want opposite
things of the same AI: one is working through something with it, the
other wants to be left alone, and the one who wants quiet should not
have to silence it for the other. So the question is asked of the
person it happens to. Turning it off changes nothing for anybody else:
they go on being answered exactly as before, and you still hear every
word of it.

**It is a filter on the turn, not on the transcript.** Everything said
in the room is written into the one conversation whoever said it, and
an AI answering somebody else reads the whole of it — the room is a
room, and a log with holes in it would be a worse thing than an AI that
heard you. What the switch stops is *your* words being the thing an AI
answers.

**Off has two meanings, and you pick which**, in Settings → *Turning
the AI off*:

- *Unless I say its name* — the default. The AI stays out of what you
  say and still answers when you call it by name, so a quiet room is
  still a room you can ask a question in.
- *Never* — nothing you say reaches it, its own name included. Somebody
  else in the room can still bring it in, and you will hear the answer.

Which of the two is this browser's, like the push-to-talk nuance above
it; which position the switch is *in* is the room's, kept against your
invite name. So it survives a rename, it is the same in every room of
the house, and it is still off when you come back tomorrow. It is on
the roster the room sends everybody, because an AI that answers one
person and not the one beside them is a room where the reason has to be
visible or it reads as a fault.

**And it is visible**: a crossed-out speech bubble against their name
in the list of who is here, in the colour the room uses for an AI.
Only when the switch is off, on everybody's screen and not just their
own — everybody starts with it on, and a mark against every name in the
room would be a mark that says nothing. The same mark for both ways of
being off, since at that size the thing to see is whether the AIs are
in what somebody says at all; which of the two it is is in the tooltip,
and in how solid the mark is.

**An AI's own strip has four things on it**, and no fader: a voice the
room made is as loud as the room asked it to be, and nobody has ever
wanted one louder or quieter than the rest of the room.

- **On / Off** switches that voice off *in this room*. Off is off: it
  is given no turn by anything, so nothing is asked of a model, nothing
  is thought and nothing is said. It keeps its place in the list and
  everything set on it, and the strip dims and says `off` — this is the
  room not having that voice in it for the moment, not a bot being
  taken out of the house. The room's, like **Stops**: the room down the
  hall has its own answer, and this one is still off tomorrow.
- **Mute** is the room *reading* that voice rather than hearing it.
  Nothing is synthesized at all — in any language — and because the
  words are no longer paced to audio that doesn't exist, the whole
  reply lands at once, as fast as it is thought. Nor can anybody talk
  over it: there is nothing in the air to talk over, so **Stops** has
  nothing to do and a sentence is never cut off half-written. **Stop**
  still stops it; that one is the room saying so.
- **Stops** and **Voice**, above.

### Games

In the lobby (the room's ordinary conversation) the AI can design a game
with you. Talk it through; it fills in what you leave open, checks the
name is free, and once you've confirmed it writes the game's prompt
itself and saves it. The prompt never reaches a browser: what appears in
the chat is a card with the game's name and summary.

Everything else is by voice too, in the lobby: ask the AI to start a
game ("let's play Quick Draw") and, once its reply has been heard,
everyone moves into a fresh conversation run by that prompt alone (the
AI opens the scene). Ask it to change a game and it reads the prompt to
itself, rewrites it, and replaces the card; ask it to delete one and it
checks which game you mean before deleting. Inside a game the AI only
plays it: leave the game to change games.

A game also carries settings, not just a prompt. Today that is
`reply_mode`, how the room is run while the game is played:

| `reply_mode` | |
|---|---|
| `room` (default) | however the people have the room set, back-and-forth and all |
| `auto` | the AI is shown every pause and judges whether to come in |
| `always` | the AI answers every pause: for a game where it deals the turns |
| `named` | the AI speaks only when a player says its name |

The AI picks one as it designs the game and can be told which you want.
While a game that sets one of the last three is being played it binds
every AI in the room; leaving the game puts them back as they were.
Three things it cannot reach: a bot set to **never**, which stays
silent in any game and is still silent when you leave it; **Stops**,
which is nobody's but the bot's; and anybody's **BOTS** switch, which
is theirs — a game decides how the room is run, not who may be spoken
to. A game also records how many players it is for (`players_min` and
`players_max`, either end open), which the AI works out with you.

A saved game is announced in the conversation it was designed in, as a
card with its name and what it is about, and that card is where it is
played from: **Play** takes everyone in the room into it, and **Peek**
shows its hidden prompt to you alone. Asking the AI for it out loud
does the same thing. (There is no menu of games anywhere: a game
belongs to the conversation that made it, and a second index of them
was a second place to look.) **Leave the game** comes back out of one;
**Restart game** starts it over. Saving needs a model with tool calling (llama.cpp
needs `--jinja`); without it the AI can talk about games but not save
them.

### Cards, just for you

Each person has their own cards, under **Just for you** — its own column
on a wide monitor, its own tab on a narrow one, and on nobody else's
screen. The AI deals them: a secret role, a hand, what only your
character notices, an instruction meant for you alone.

The area comes and goes with the cards. Holding none, you have no
**Just for you** at all: not a column explaining what might one day turn
up in it, just the conversation with the room it was taking. The first
card dealt opens it; the last one taken back closes it again, and if you
were looking at its tab at the time you are put back on the
conversation.

They are *cards*, and the prompt tells the AI to call them that out loud
— "Ryan, check your cards" — because the obvious name for a region of
somebody's screen is a phrase no voice should ever say to a room.

What is in that area is the AI's to decide, and only the AI's — there is
no way for the person holding a card to clear it. The four tools, in the
lobby and in a game alike:

- `list_people` — everyone on the roster, with the exact name to address
  a card to, whether they are here with a mic open, and the titles of
  what each is already holding.
- `send_card` — a title and a body, to one person. Sending a title they
  already hold **rewrites that card where it sits**, which is how a hand
  or a standing order changes without stacking copies; the card glows
  again so they notice, and the footer says it changed rather than
  arrived.
- `take_back_card` — removes one by title, once it no longer applies.
- `read_cards` — what it has given out, in full. A long game outruns the
  conversation the model can still see, and a game master that has
  forgotten who the traitor is cannot run the game.
- `post_card` — the opposite: a card written into the **conversation**,
  for the whole room, which stays there and cannot be taken back. It is
  where anything everyone should be looking at belongs — the rules as
  agreed, the standings, the cast of a scene — and it saves the AI
  reading a list aloud. It is shown to the model as its own, in full, so
  it keeps playing by what the room can see. Every AI in the room has
  this one, including the ones that may do nothing else: writing
  something for the room to look at is saying it.

**A card is written in markdown**, and it is the only thing in the room
that is — everything else here is written to be heard. Tables, bullet
and numbered lists, headings, **bold**, `code` blocks, quotes, rules and
links; no images, nothing that fetches anything, and no raw HTML at all.
That is what makes it worth having beside a voice: ask for the weather
and the AI can say "cold tomorrow, wet on Friday" while the figures go
up in a table, which is a thing speech is bad at and a card is good at.
The prompt asks for both — the card is not instead of answering, and
nobody hears a card appear, so it says that it is there.

A card's body is written by a model, so **nothing in it can become
markup**. It is escaped before a single tag is built from it, every tag
in the result was put there by `web/src/lib/markdown.js`, and a
`<script>` in a card is three words on the screen. A `javascript:` link
is not a link; an `http` one opens in a tab of its own. The `cards`
suite asserts exactly that, against a card written to try it on.

It has one more tool everywhere, `wait_until_named`, which is how it ends
a back-and-forth by itself (above).

Nothing about a card is broadcast. The server tags the event with the one
person it is for and writes it only to their own sockets, so another
browser never receives the text at all — the secrecy doesn't depend on
the page hiding anything. A card is silent, so the AI is told to say that
it sent one without saying what is on it ("Ryan, take a look at your
card"), and the sender's own voice is heard by the whole room, so it
never reads one aloud.

Cards belong to the conversation they were dealt in: **New conversation**,
**Restart game** and moving between the lobby and a game all clear
everyone holding nothing. Short of that, a card stays until the AI takes
it back.

### What a shared desktop costs

The thumbnail is small (320 px wide, 2 frames a second, VP8) and goes to
everyone all the time, which is what makes the mixer live rather than a
list of names. The full size is only *encoded* while somebody has it
open: the relay tells the sharer's browser whether anybody is looking,
and with nobody looking it isn't made at all.

The server never decodes any of it. It forwards frames and keeps one
rule — **a viewer is sent no delta frames until a keyframe has reached
them** — which is what makes dropping safe: a viewer who falls behind is
put back to the next keyframe instead of being shown a torn picture, and
the sharer is asked for one. That is also how somebody who arrives
mid-stream fills in.

It is a second WebSocket (`/screen.ws`), and the separate connection is
the point: a burst of video in the audio socket's send queue would be
head-of-line blocking applied to speech. Proxy it like `/room.ws`.

What this does not have is WebRTC's congestion control. The rates are
fixed, so a desktop that is mostly still costs little and a bad network
degrades without a feedback loop to degrade *gracefully*. If that ever
matters more than the simplicity does, the whole of it is `screen.rs`
and `web/src/lib/screen.svelte.js`, and nothing in the audio path knows
it exists.

Sharing needs WebCodecs video (`VideoEncoder`), so the button is only
offered where the browser has it. Watching needs `VideoDecoder`.

### What judging a pause costs

"When it fits" asks the model at every pause, so a conversation between
two people is a model call every time they stop for breath — a whole
prompt in, usually a single tool call out. That is the trade: the
judgment is a semantic one ("is this mine to answer?"), which is what a
language model is for, and asking the same model that would answer means
a pause it *does* take costs nothing extra — it just keeps talking,
with no second round trip.

A bot set to finish judges one pause it would otherwise have answered:
the one made by whoever talked over it. That is the same model call it
was going to make, asked a different question, so finishing costs
nothing extra — and a cough it decides against costs one tool call
instead of a whole spoken reply.

If that turns out to be too expensive, the seam to replace is narrow: the
room hands out a turn with a `Cue::Decide`, and anything that can answer
"speak or stay quiet" can sit behind it — a small local model, or a
turn-taking model, without the room or the UI changing.

The judgment is only as good as what the model is told, and the failure
is silent: a model that is told a pause is probably not its own says
nothing, for ever, and the mode looks broken rather than wrong. There is
a test for it that puts real pauses to the configured endpoint and checks
which way it goes, off by default because it costs what that endpoint
costs:

```bash
cargo test -p vox_room -- --ignored --nocapture judges
```

Run it after changing anything the model reads. There is a second live
test, `whole_reply`, that puts a real reply through the whole path — the
room's own request, the stream parsed as it arrives, a tool run from it,
and the model carrying on from the tool's answer.

A third, `says_hello`, says hello to the picture server the
environment points at and reads back what it lends. It generates
nothing, so it costs no GPU time, and it checks the half that a made-up
server cannot: the SSE framing a real one answers with, the session it
hands out and expects back on every later request, that the tool list
really is filtered down to what was asked for, that no key is in any
schema the model would be shown, and that the words each tool draws
from are findable under one of the names the room looks for — without
which no card goes up and no picture gets a caption.

It needs the key: unlike the hosted server the room was first built
against, this one answers 401 to `initialize` itself. It reads the same
two variables the room does, so an environment that runs the room runs
the test:

```bash
VOX_ROOM_MCP='art=https://comfyui-mcp.example.com/mcp=generate_pixel_art,refine_pixel_art,list_styles' \
VOX_ROOM_MCP_KEY=… \
  cargo test -p vox_room -- --ignored --nocapture says_hello
```

### Faces

Everyone in the room has a square beside everything they say, and so
does each AI. Until one is made it is the tint that name already has
with their initials in it, which is the same size and shape as the
picture that may replace it — the column of names lines up either way.

A face is **made by asking for one out loud**, or **dropped in from
your own computer** — see [Dropping a picture
in](#dropping-a-picture-in). Asking for one takes four steps on
purpose — the AI does not simply hand you one.

It interviews you first: who you are, what you are wearing, what sort
of face — and, where the server draws in more than one look, which
look, because that is the one thing about a picture that no later
change can give you. Then it says what it is about to make and makes
it — and the
picture goes **into the conversation as a card**, numbered, with a
spinner on it until it arrives. Generating one takes the better part
of a minute, so the card is the room's evidence that something is
happening.

Then you look at it and say what to change. Another picture is another
card with the next number, and the earlier ones stay where they are,
so *"go back to the second one"* is a thing you can ask for. The AI
cannot see any of them — it is told the numbers and nothing else — so
the room is its eyes.

Only when you say you are happy with one does it call `set_avatar`
with that number, and the square beside everything you say becomes
that picture. Clicking any face opens **who that person is**: the
picture at a size worth looking at, the few lines the AI wrote about
them, what the picture was asked for, and what the house knows — the
language they read in, whether the room is theirs, whether they are
here now. The bio is written by voice too, with `set_bio`, and can be
revised without making a new picture.

The pictures stay in the conversation they were made in and go when it
does. A face does not: it is copied when it is set, so deleting the
conversation you sat for leaves you looking the same.

A picture that was still being drawn when the room stopped is marked
as never having arrived, on the way back up — the call it was waiting
on died with the process, and a spinner is a promise that something is
still happening.

**A picture is nobody's until it is assigned.** The AI can put any
picture in the chat on anyone here — a person, itself, or another AI
in the room — so the room can look at one and then decide whose it is,
and a picture somebody doesn't want is one to offer around rather than
throw away. (Only the first AI can; the rest can look and talk. See
[More than one AI in a room](#more-than-one-ai-in-a-room).)

**Settings → Your face** keeps the last ten you have worn and offers
them back. Choosing one again costs nothing and needs no AI, so it is
a row of thumbnails and a press rather than something to ask for out
loud. Taking your face off leaves it in that row — it is not the same
as never having had one. Yours is the only one you can change this
way; not even the owner can reach somebody else's.

Faces appear beside every line in the conversation and beside every
name in the list of who is here, and clicking any of them opens that
person's card.

#### Dropping a picture in

**Drag a picture onto the room and let go**, or **paste one with
Ctrl+V** — a screenshot is on the clipboard as a file, so the two are
the same act. It goes into the
conversation as a card with your name on it and the next number — the
same numbering the AI's own pictures use, because *"the second one"*
is said out loud about whatever is on screen and nobody counts the two
apart. From then on it is a picture like any other: ask the AI to make
it your face and it calls `set_avatar` with that number, and the
conversation keeps it the way it keeps everything else said in it.

The AI is told that it is there, whose it is, and what the file was
called. It is **not** shown the picture — it is shown no picture, its
own included — and it is told plainly that it did not make this one,
so it asks about it rather than describing a thing it cannot see.

A paste that is text *with* pictures in it — part of a web page, say —
is left to whatever has the cursor. That one is meant for the box you
are typing in, and taking the pictures out of it would be answering a
different question; a screenshot carries no text with it, which is how
the two are told apart.

Only pictures. Anything else is declined where you dropped it, with a
word in the footer, and nothing is uploaded. Whatever your browser can
decode is a picture here — a JPEG off a camera, a PNG, a WebP — and
the tab re-encodes it before it is sent: scaled down to the size the
room actually shows a picture at, and written out as a PNG, which is
what everything here that holds a picture has always been. So a
photograph straight off a phone comes in as a few tens of kilobytes
rather than being refused for being what phones make. The server keeps
nothing over 1 MB, which is also the most a face may be: anything in
the chat can be worn.

A room with no picture server can do all of this. Nothing here draws,
so the AI is not told how to draw — but it is still told about faces,
and `set_avatar` still has something to put.

#### Changing a picture you dropped in

Asking for a hat on a picture you brought in used to get you an
apology. A tool that changes a picture only knows the pictures its own
server drew — it is given the handle that server wrote on the card,
not the number the room shows — and a photograph off your phone is a
stranger to it. On screen the two kinds of card look alike, so *"put a
hat on that one"* worked or didn't depending on where the picture came
from.

It works now, with a step in front of it that the AI says out loud
first. **`redraw_picture` hands your picture to the picture server**,
which redraws it in one of its looks and keeps it from then on. What
lands is a new card with the next number, with a handle on it, and
that one can be changed like any the AI made. Yours stays in the
conversation exactly as you left it.

Two things are worth knowing before you say yes. What comes back is a
**redrawn copy in the server's look** — it is not your photograph with
something added, and which look it is drawn in is asked about like any
other picture, because no later change can alter it. And it takes as
long as making a picture does, so the AI says what it is doing and the
card spins while it happens.

None of this is needed to *use* a picture you dropped in. `set_avatar`
takes it exactly as it came, which is the right answer when you like
it already.

The two tools the server does this with — one that hands out a
one-time upload URL, one that takes what was posted — are **not lent
to the AI**, and naming them in `--mcp` does not lend them either. The
step between them is a file going up a socket, which no model can do:
the room posts it, from the picture it already has in its database.
Where that file may be posted is checked first — the configured
server's own address, or https, and nowhere else — because the URL
arrives inside a tool result and what is going up is your photograph.
A server that publishes neither tool is a room with no
`redraw_picture` and nothing in the prompt about it.

Pictures made by the AI come from an **MCP server**, which is any
server the house was started with:

```bash
vox_room --room Lobby \
  --mcp 'art=https://comfyui-mcp.example.com/mcp=generate_pixel_art,refine_pixel_art,list_styles' \
  --mcp-key "$ART_KEY"
```

`NAME=URL=TOOL,TOOL,...`. The name is the prefix its tools are offered
under — `art` gives the AI `art_generate_pixel_art` — so two servers
lending a tool of the same name stay apart.

**The name is also how a key finds its server.** `--mcp-key` takes
`NAME=KEY`, semicolon-separated, one per server:

```bash
vox_room --room Lobby \
  --mcp 'art=https://comfyui-mcp.example.com/mcp=generate_pixel_art;docs=https://docs.example.com/mcp=search' \
  --mcp-key "art=$ART_KEY;docs=$DOCS_KEY"
```

A key written on its own, as in the first example above, is shorthand
for "the only server there is" — and with more than one server it is
refused rather than guessed at, because one vendor's key is not a
thing to hand to another. A server nobody gave a key simply gets none,
which is right for one that wants no authenticating.

**The tool list after the URL is the point of the flag.** A picture
server offers everything it can do, and a room that wants faces needs
three of them: one that makes a picture from a description, one that
changes a picture already made, and one that says what looks it can
draw in. The ComfyUI server above offers eight, and the five left out
are two the room takes for itself and calls without lending (the ones
that bring a picture you dropped in to the server — see [Changing a
picture you dropped in](#changing-a-picture-you-dropped-in)), one that
re-posts a picture already in the chat as a second card, one for
polling a job you only need if you told it not to wait, and one that
lists every job on the server — including other rooms'. Each of those is one more way for a voice answering "make
me an avatar" to end up somewhere else. Leave the list out and every
tool the server has is offered, which is only sane for a server you
know is small. Repeat `--mcp` (or separate with `;`) for more servers.

**What looks there are is the server's to say.** That ComfyUI server
draws in two as it stands — a 64-pixel sprite, one colour to a pixel,
and a 768-pixel cel-shaded portrait with ink lineart — and which one
is an argument of the tool that draws, with the names in its own
schema. The room names neither, anywhere: not in the prompt, not in
the code, not in the browser. A room that listed them would be wrong
the next time whoever runs that server adds one, and the AI would be
confidently offering a look that no longer exists. What the room says
instead is that the choice is yours and is made before anything is
drawn, and `list_styles` is how the AI finds out what it is offering
you. That tool draws nothing, costs no GPU time, and gets no card —
asking what there is is a question in the middle of an interview, and
a spinner for it would promise a picture nobody is making.

Changing your mind about the look costs a whole new picture. A tool
that changes one keeps the look it was drawn in — that is what makes
it a change rather than a fresh start — so the look is settled in the
interview and not afterwards.

Only streamable HTTP is spoken, not stdio: a stdio server would want a
child process supervised for as long as the house is open.

A server may also want a session, handed out in its answer to
`initialize` and expected back on every later request; the room keeps
whichever one it is given. The ComfyUI server does this and Retro
Diffusion does not, and neither needs saying in the configuration.

**The key never reaches the model.** Some servers take the key as an
argument rather than a header — Retro Diffusion's tools want an
`rd_api_key` — and a model shown a field like that fills it in: with
something it invented, or, worse, correctly, which would put the key in
a transcript that is written down and shown on screen. So the key is
stripped out of every schema the AI is shown and filled back in when
the call is made, from `--mcp-key`. A server that wants the bearer
header and nothing else, as the ComfyUI one does, never has anything
stripped.

**What a picture costs is the server's business, not the room's.** A
server on a machine of your own costs GPU time and a hosted one bills
per image, and the AI is told neither, because the room cannot know
which it is pointed at. What it is told is the wait — ten seconds or so
for a sprite, the better part of a minute for a refine, with the room
listening to nothing throughout — which is why the prompt has it say
what it is about to make before it makes it, and make one at a time.

**A card remembers what the tool said when it made it.** A tool that
changes a picture has to be told which one, by a handle that is the
tool's own and not the number on the card — and it says that handle
once, answering the call that drew the picture. A tool result does not
survive the turn it arrived in: what a model is given next time is the
conversation, and the conversation is what was said out loud. Since
nobody asks for a change in the same breath as "here it is", the
handle would always be gone by the time it was wanted. So the tool's
words are kept beside the card and put back in front of the model
every turn after, word for word — the room cannot know which server it
is talking to, and what the tool said is what the tool will understand
coming back. Capped at 300 characters, and never sent to the browser:
it is machinery, and nobody is going to read a job id out loud.

Making a picture and giving somebody a face are deliberately two steps:
the server's tool makes it, and the room's own `set_avatar` puts one of
what it made on a person. What a picture is kept in and how it is
cached is under [How it works](#how-it-works).

A face belongs to the **person**, not to the room or the conversation:
it follows them between rooms, outlasts the conversation, and survives
renaming themselves. **Settings → Your face** takes yours off again,
and yours is the only one you can take off — not even the owner can
clear somebody else's.

### Voices

With a **voice-cloning server** configured (`VOX_ROOM_TTS_CLONE_URL` —
the `clone` service of `qwen3-tts/docker-compose.yml`, which holds the
one Qwen3-TTS model that can take a reference clip), the AI can record
a few seconds of somebody in the room reading aloud, keep a voice
cloned from it under a name, and be asked to speak in it. Without one
it has none of those tools and nothing in its prompt mentions them.

Ask for it out loud. The AI says what it is about to do, then puts a
**card with a passage on your screen** and the room starts listening —
and a card goes into the conversation, in your name, with a red dot on
it saying it is recording. **No microphone here is recorded without a
light on it**, and the light is in the conversation where the whole
room can see it rather than on the screen of the one person being
recorded.

Then read the passage. The clip starts on your first word — the room
keeps the moment before it, so nothing is clipped — and ends when you
stop. On push to talk it ends when you let go of the key, which is the
cleanest end there is: you stopped reading and said so. Two words and a
pause is not a reading: *"go on then"* is forgotten and the room keeps
listening, and so is letting go of the key after two words. Nothing at all for two minutes and it gives up, and the card
says so. The clip is capped at thirty seconds, and a voice is cloned
from a good deal less.

Being muted is not a refusal when push to talk is on — that is what
push to talk *is*, muted between every word — so the AI can arm the
recording and you hold the key when you are ready. Muted with push to
talk off is refused, and so is a strip turned off on the room's own
mixer, which is not yours to unmute.

**What you are reading is a clip and not a turn**: the AI is not handed
it as something said to it, even in a room where it answers every
pause. It gets the turn when the clip lands, with the card in the
conversation in front of it — how long it is, and what you said. Then
it asks what to call the voice, and `save_voice` keeps it under that
name.

The clip's **transcript goes with it**: the server clones better from a
few seconds of speech when it is told what the speech says, and the
room has that already from the transcription. Where nothing was
written down, the passage stands in for it.

**A clip can also be brought in.** Drop a recording onto the room — or
paste one — and it goes into the conversation with the next number on
it, playable, and `save_voice` takes that number exactly as it takes a
read one. The browser decodes whatever the file was (mp3, m4a, ogg, a
voice memo off a phone), because it already has a decoder for everything
it can play; the room makes it mono at 24 kHz, bounds it to thirty
seconds, and reads it with the same model that does the room's own last
pass — so the clone server is told what the clip says rather than
cloning blind.

**It is not dressed up as a recording.** A brought-in clip is its own
kind of card, saying so and naming the file it came in as, and the AI is
told plainly that nobody read this one here — otherwise it thanks
somebody for reading a file they already had, and the room claims
something happened in it that did not. The clone server's own consent
field records which of the two kinds a clip was, since it asks for one
and that is what the house actually knows.

A kept voice is **the house's**, under the name you gave it: any room
can ask the AI to speak in it, `list_voices` says what there is, and
`forget_voice` deletes one for good — the server's copy first, so
nothing is left named after audio that has gone. The clip itself stays
in the conversation it was read in, with a play button on its card, and
goes when that conversation does.

**Asking for one out loud is temporary.** `speak_as` changes which
voice the AI speaks in from its next sentence on, and its strip says
*Speaking as Ryan* for as long as it lasts. That one is the
afternoon's: it is not written down, and the next restart undoes it. A
voice deleted from under a bot that is wearing it leaves that bot in
its own voice rather than silent.

**Settling one is not.** **Settings → Voices** has a picker per AI:
its own voice, or any the house has cloned. What you choose it wears
from that moment, keeps, and comes back up in after a restart — so an
AI that should always sound like somebody is set once rather than asked
every session.

**That choice is the house's**, and a room may disagree with it. It is
the same AI in every room — one name, one face, one voice — so settling
it in Settings settles it everywhere, there and then: the other rooms
put it on without waiting for a restart. Anybody in the room may set
it, like the faders: the control is no more than the sentence you could
say instead.

A room that wants a different reader without changing the AI down the
hall sets one on the AI's strip, under **Voice**: *as the house has
it*, which is where every room starts and what most of them stay, or
its own voice, or any the house has cloned. A room with an answer of
its own keeps it, and the house being settled afterwards goes past it.
*As the house has it* hands the question back — which is not the same
as choosing the AI's own voice here, that being an answer like any
other.

The room's is read first and the house's is the fallback, so the thing
nobody has to set is the thing most rooms want.

The two ways do not fight. Ask it out loud for a different voice and it
obliges — in that room, for as long as the house is up — and the one
you settled on is what every room opens in next time. A voice that is
deleted is unsettled everywhere at once, and taken off any AI that was
speaking in it in any room: the setting names a voice rather than
holding it, and a bot left holding the id of a clone the server no
longer has is a bot whose next sentence fails instead of coming out in
its own voice.

A puppet has no picker: its voice is the part it plays, decided when
the character was made up. See [Puppets](#puppets).

Each AI wears its own, and only the first one can record anybody or
keep a voice — the others can look and talk, like everything else
under [More than one AI in a room](#more-than-one-ai-in-a-room). A
voice it has been lent is spoken in every language the room is
listening in: the bot changes voice, not one of its languages.

The clips are 16-bit mono WAV at 24 kHz — Qwen3-TTS's own rate — kept
as blobs in the database beside the pictures, and fetched by whoever
presses play. Anyone invited can play one, which is no more than they
could already hear: it is what somebody said out loud in a room with
everybody else in it, and the transcript of it is on every screen
there.


### Puppets

A bot has a brain of its own. Two of them in a room are two models
talking to each other, a request each per line, each deciding for
itself what to say next — which is the point of them, and which is also
why they cannot act out a scene. Neither one knows what the other is
about to say, so there is no timing to it: every line waits for a model
to finish thinking, and an interjection lands wherever the round trip
put it.

A **puppet** is the other half of that. It has a name, a face, a voice
and a fader of its own — to anybody in the room it *is* another voice
in the room — and nothing behind it at all. What a puppet says is the
first AI's, written in advance and said through it. **Nothing ever
gives a puppet the turn**, because there is nothing there to take one:
it answers nothing, and that is not a setting anybody chose. Saying a
puppet's name gives the turn to the AI that holds it, which is how a
puppet left standing after a show takes a question.

That is what makes a **puppet show** possible. The AI writes the whole
scene first, every line of it; the room turns it all into speech
*before* it plays — a line or two ahead, so the audio for line four is
made while line three is being heard — and then the lines land exactly
where the script put them. A beat of silence. An answer coming in on
the last word. Two voices over each other for half a second. **The
timing is the whole reason to do it**; a play made up a line at a time
is not worth the puppets.

**Every line is spoken in full**, to its last syllable. An interruption
is not something the room does to a line; it is something the script
*writes*: the interrupted line stops where the character got to and
ends with a dash — *"But I only wanted—"* — and the line after it comes
in over the tail. The room used to do it the other way, synthesizing a
whole line and then cutting the audio off with the same machinery a
person talking over a bot uses, and that was wrong twice over. The cut
landed where the wall clock said rather than where the writing meant,
and most of a line the speech server had been paid for was thrown away.
A playwright knows that the sentence breaks after *"wanted"*; nothing
else does.

It needs a speech server (`--tts qwen3`). Without one there are no
puppets at all and nothing in the AI's prompt mentions them: a puppet
with no voice is a name on a fader.

**Making one.** Ask out loud. `make_puppet` keeps a character under a
name, with a line about who it is and a line of **delivery notes** —
written as instructions to an actor, *"gruff and slow, an old soldier
who has told this story before"*. That line is what makes a puppet
sound like itself rather than like the AI, and it is the usual answer in
a house with no cloned voices: the delivery goes to Qwen3-TTS as its
`instructions`, over whichever of the server's voices the character was
built on (`VOX_ROOM_TTS_VOICES` says what there is). Where somebody has
read for a [cloned voice](#voices), a puppet can have that instead.

A puppet wears a face the way everybody in the room does: make a
picture, show it to the room, `set_avatar` it onto the puppet by name,
`set_bio` writes who it is. **The name, the voice, the face and the bio
are the house's from then on** — `list_puppets` is the roster, and a
character made one evening can be cast again months later without being
made again. `forget_puppet` deletes one for good, and takes every face
it ever wore with it: a name that is free again must not hand the next
character somebody's old portrait.

The stage is not the roster. `bring_on_puppets` puts a cast in the
room, where everyone watches them arrive and can see who is about to
speak; `send_off_puppets` takes them out again, and what they said stays
in the conversation with their names on it, because that is what
happened. Six at once is the limit, which is a limit on what an
audience can follow rather than on anything the server minds. **The
stage does not survive a restart** — the roster does. A puppet is
something the room is doing, like a game; the characters are something
the house keeps, like the voices.

**Setting a show up** is a conversation, and the AI is told to have it
in order: who is in it, from the roster or made now; what it is about;
and only then the script. `put_on_show` takes a title and every line in
one call — `who`, `say`, and the timing. `pause` is the seconds of
silence before a line, measured from the moment the one before it was
heard out. `over` is the other kind: the line comes in that many
seconds before the previous one ends, so both are heard at once for
that long, which is what an interjection sounds like. Half a second is
somebody landing on your last word. A line may be the AI's own, so it
can take a part in its own play. It may never be another bot's: that
one has a brain of its own and will answer for itself.

### A playhouse in three parts

**A show is on whenever a character is standing in the room** —
`bring_on_puppets` opens the playhouse and the last `send_off_puppets`
closes it. A script is something that happens *inside* a show rather
than what a show is: between two scenes and all through the questions
afterwards the puppets are still there, and so is everything below.

An open playhouse has three parts, stacked above the conversation, and
which one you are in decides who can hear you:

| | |
|---|---|
| **Stage** | the characters, at twice the size they are in the conversation, with the one speaking lit — and the lines they are saying, in a log of their own. Going up there is joining the play |
| **Director's box** | where the AI running the show sits. What is said here is the only thing it hears — and it is **read here too**, in a log of its own under the faces, not down in the conversation |
| **Audience** | the conversation, and everybody watching. Talk as freely as you like |

**The box and the stage cannot hear the audience.** That is the whole
rule and it refuses in one direction only: the audience can follow the
play and the talk in the box perfectly well. It is the point of the
thing — a play with the audience shouting suggestions at the director is
not a play — and a suggestion reaches the director the way it does in a
real theatre: somebody goes round to the box. **Sit here** on a band
moves you, and the one seat nobody may move is the director's.

It is not a politeness. The audience's lines are not in the AI's
transcript at all, because a model shown a question and told to ignore
it answers it.

**The box and the audience keep separate logs.** Every entry is marked
with the part of the playhouse it was said in, at the moment it was
said, and that mark never changes — so the box is handed the box's log
and the stage's and nothing else, line by line. It is *where it was
said* and not *who said it*: somebody who comes round to the box from
the audience does not thereby say everything again from inside it, and a
director handed their last hour out front because they came for a word
would be the audience being heard through the back door. Going back out
to the audience does not take the words they said in the box with them
either.

Anything said with no show on is said in a room that has no parts to it,
and everybody hears it however the room is divided later: the hour of
conversation before the curtain went up is the director's to read, since
there was no box to be shut out of. And everything the room *posts* in
front of everybody — cards, pictures, voice clips, the programme at the
curtain — stays in the conversation whoever posted it. A card held up in
a room is seen rather than heard, and the rule here is about hearing.

When the last character leaves, the playhouse goes and the box goes with
it: what was said up there drops into the conversation on everyone's
screen, so the whole show reads back in order afterwards. Nothing is
hidden by any of this — the audience can read the box's log in the box
while the show is on, and the conversation afterwards. What is kept
apart is kept apart *for the AI*, which is the only reader a play
depends on not hearing the stalls.

Somebody who goes up **onto the stage** is joining the play. Whatever
script was running **stops where it is** — the remaining lines are
dropped, since the puppets have to start answering the person who is up
there rather than saying what was written before they were — and from
then on it is an **adlib**: no script, the puppets improvising a line at
a time through the AI that holds them. The stage says which of the two
it is on the line beneath it.

Seats are not written down. They last as long as the show does, like the
stage itself: the director takes the box when the curtain goes up,
everybody else starts in the audience, and the last character leaving
puts the room back to being a room.

**A play is not the room talking.** The puppets' lines are read on the
stage and never enter the conversation; what the conversation keeps is
one note at the curtain, under the AI's name, **folded up with the whole
script inside it**. A scene is a few hundred words of somebody else's
dialogue, and left lying open in the middle of yours it would push
everything the room actually said off the screen.

**While a script plays, the room is set for it.** The AI and every voice
in the room go to answering **nothing** and **Stops: when finished**
the moment the curtain goes up, and back to exactly what they were when
it comes down. Both are *set*, visibly — the strips say *In the show*,
and Stops is the control anybody could have set themselves — because a
scripted exchange that stopped dead when
somebody in the audience cleared their throat is not a play, and a
director that broke off mid-scene to answer a question is not one
either. A show is a thing the room is doing and it binds every bot in
it, the way a game's reply mode does.

A line in the room list says what is playing and how far in it is.
**Stop** brings the curtain down — that button is never a game's or a
show's to take, and a play set to finish whatever anybody says is
exactly the thing somebody needs a way out of. The settings are written
down as well as set, so a server that stops in the middle of the second
act does not come back up with its AI silenced for good.

**When it ends** the AI is given the turn to say so, with a card in the
conversation saying what played and who is still standing there, and the
room is its own again: it answers, and it can be interrupted. If the
room would rather ask the puppets things than send them away, they stay,
and `say_as` says one line as one of them, now — rendered first and
queued second, so two puppets answering in turn have no hole between
them. Anything said while the show was on is still in the conversation
waiting to be read, which is how the AI knows what the audience made of
it.

The puppets speak every language the room is listening in, like the AIs
do: each line is translated and synthesized ahead of time in each of
them, and the timing follows the room's own language.

### The look

The room comes up in **Obsidian Comms**: dark glass panels with their
top-left and bottom-right corners cut, laid on a near-black field and
lit along their edges in periwinkle. The AI speaks in violet and you
speak in amber, so a glance down the conversation says whose line is
whose without reading a name; everybody else keeps the hue their own
name hashes to, lifted to read on that ground. Nothing in it is a
picture — the brushed look is a gradient, the cut corners are a clip
path and the hatching is a repeating one — so it costs no requests and
is sharp at any size.

**Plain** is the other one: paper by day and graphite by night,
following whatever the machine is set to, flat, with the columns told
apart by a hairline and the page running to the glass. It is the look
the room had before it had a choice, kept whole.

**Settings → The look** picks. It is this browser's own and nothing
about it goes to the server: two people in the same room can be sitting
in two different ones and neither can tell, which is also why it is not
in your profile — it is a fact about a screen, not about a person, and
the same person at a desk and on a phone may well want different
answers.

**The corner grammar.** A theme declares two cuts and no more, and
everything in the room that has a cut corner wears one of them:

| cut | where | Obsidian |
| --- | --- | --- |
| `--cut-panel` | a column, the conversation, the footer, the heading that names the room, the door | 16px off the top left and bottom right |
| `--cut-card` | a panel inside a panel — a fader, the note under them | the same, at 9px |

Each is a pair: `--cut-X` is the shape as a `clip-path`, and
`--cut-X-edges` is the line along the faces the cut opened. That line
has to be a background layer rather than a border, because `clip-path`
takes a bite out of the very rectangle a border is tracing — the cut
face is the one edge nothing else will draw. Obsidian writes the
diagonal once, as `--cut-face`, and places it in the corners; the
colour is `--cut-ink`, substituted where the token is *used*, so the
one gradient comes out in the frame's colour on a panel and the card's
on a card.

**Two, and not on everything.** A cut corner says *this is a piece of
the console*, and the pieces are the panels. Buttons and message
bubbles are the things inside one — they are small and there are a
great many of them, and machining those as well turned every paragraph
and every control into another bit of equipment to look at. They are
round, from `--btn-radius` and `--bubble-radius`, and the geometry is
left to the frames.

A cut is a clip, and a clip takes anything drawn *outside* the box with
it — a focus ring, a spread shadow, and its children's too.
`--focus-inset` is why the ring is pulled inside a cut thing, the
heading that names the room being a button that is one.

**Writing another.** A theme is a block of custom properties keyed on
`data-theme`, in `web/src/themes/`. `tokens.css` is the vocabulary —
about eighty of them, every one declared with a value, so a theme that
leaves one out gets that rather than a blank. A theme sets tokens and
nothing else: the shapes are drawn once in `chrome.css` out of the same
tokens, so four values change the frame on every panel in the room
rather than nine components each having their own. Components read
tokens and nothing else; one hardcoded colour is one place the next
theme is wrong. Add the file, `@import` it from `app.css`, and put its
name in `THEMES` in `lib/theme.svelte.js` — that list is what Settings
offers. `legacy.css` is forty lines and is the one worth copying.

## How it works

- One WebSocket per tab carries 20 ms mono Opus frames both ways
  (WebCodecs in the browser, libopus on the server) and JSON room events.
  A tab sharing or watching a desktop has a second one, `/screen.ws`,
  that carries VP8 frames and nothing else.
- The server mixes on a 20 ms clock. For each listener it sums everyone
  except that listener, plus the AI in that listener's language, and
  encodes it for them alone. The AI's languages are alternatives rather
  than parts of a mix: one queue each, and you hear exactly one of them.
- Each person's mic feeds its own scribe transcription engine (streaming
  text, then the offline re-decode), so their lines are labelled by who
  said them with no diarization.
- When nobody has spoken for `--reply-after-ms` (1.2 s), what was said is
  handed to the conversation, and the AI may answer. Its reply streams
  from the model a sentence at a time into the TTS (the first clause on
  its own, so the voice starts sooner), and each synthesized sentence is
  queued in the mixer.
- Every reply logs where its time went, as `reply:` lines: how long after
  the last words it was asked (`after_words_ms`), when the model's stream
  opened and its first words and first sentence arrived, how long each
  sentence took to synthesize, and when its first audio played.
- The mixer counts how much of each sentence it has played. That count,
  not the model's text, is what the conversation records: whole
  sentences, then the words of the current one that have been heard in
  full.

### Thinking, and which API

Two different things used to share one name, so they now have two.

`--reasoning none|low|medium|high` (default `low`) is how hard the model
may think before it answers. Note the default: before, thinking was off
unless you asked for it, and now a little of it is on. `--reasoning none`
is the old off. On a local server it turns the chat
template's thinking on or off (`chat_template_kwargs.enable_thinking`,
which is a llama.cpp and vLLM convention and nothing else knows it); on
OpenAI it is the reasoning effort. `--think` is the old name for
`--reasoning medium`, and `VOX_AGENT_THINK` is still read — leniently,
since as a flag anything but exactly `true` or `false` used to stop the
room from starting.

`--llm-api auto|responses|chat` is which API to speak. It matters
because **OpenAI's reasoning models refuse function tools on
`/chat/completions`** unless the thinking is switched off entirely — and
the room puts tools on every reply, for the cards and the games — so on
that API the AI runs every game, and judges every pause, without
thinking. `/responses` takes both together. A local llama.cpp or vLLM has
only `/chat/completions`.

`auto` asks `/responses` first and remembers, once per run, if the
endpoint hasn't got one, so neither kind needs configuring. Against
OpenAI it is also the faster of the two: thinking at `low` through
`/responses` beat thinking switched off through `/chat/completions`, which
pays for a rejected request before every reply.

## Options

`vox_room --help` lists them all; each also reads an environment variable.

| Variable | Default | |
|---|---|---|
| `VOX_ROOM_LISTEN` | `127.0.0.1:7340` | address to serve on |
| `VOX_ROOM_URL` | the listen address | public address, for invite links |
| `VOX_ROOM_DATA` | `~/.local/share/vox_room` (macOS: `~/Library/Application Support/vox_room`) | the database |
| `VOX_ROOM_AGENT_NAME` | `Vox` | the AI's name |
| `VOX_ROOM_AGENT_ALIASES` | | other spellings that count as its name, comma-separated (what speech-to-text hears, e.g. `box,fox`) |
| `VOX_ROOM_BOTS` | | more AIs in every room, `NAME[:VOICE[:PROMPT_FILE]]`, comma-separated. They see each other and answer to their own names; the first AI is the one that runs the room. See [More than one AI in a room](#more-than-one-ai-in-a-room) |
| `VOX_ROOM_REPLY_AFTER_MS` | `1200` | quiet before what was said is handed over |
| `VOX_ROOM_CONFIRM_MS` | `300` | continuous speech that cuts the AI off |
| `VOX_ROOM_ENGAGED_FOR_MS` | `30000` | idle time that ends a back-and-forth with a bot that was waiting to be named |
| `VOX_ROOM_AI_TURNS` | `2` | replies the AIs may give each other with nobody human saying anything in between; reset whenever somebody speaks or types. 0 is AIs that only answer people |
| `VOX_ROOM_HISTORY` | `60` | entries of the conversation sent to the model |
| `VOX_SCRIBE_LLM_URL`, `VOX_SCRIBE_LLM_MODEL`, `VOX_SCRIBE_LLM_KEY` | as for scribe and `agent` | the model |
| `VOX_ROOM_REASONING` | `low` | how hard it thinks: `none`, `low`, `medium`, `high` |
| `VOX_ROOM_LLM_API` | `auto` | which API: `auto`, `responses`, `chat` |
| `VOX_ROOM_PROMPT`, `VOX_ROOM_PROMPT_FILE` | a short one for spoken group chat | system prompt; `{name}` and `{people}` are filled in |
| `VOX_ROOM_TTS` | see above | `qwen3`, `kokoro` or `none` |
| `VOX_ROOM_TTS_URL` | `PI_TTS_URL` | Qwen3-TTS server (`/v1/audio/speech`) |
| `VOX_ROOM_TTS_CLONE_URL`, `VOX_ROOM_TTS_CLONE_KEY` | | a voice-cloning server (the `clone` service of `qwen3-tts/`), and its key if it isn't `VOX_ROOM_TTS_KEY`'s. With one the AI can record somebody reading aloud and speak in a voice cloned from it; without one it has no such tools. Needs `--tts qwen3`. See [Voices](#voices) |
| `VOX_ROOM_VOICE`, `VOX_ROOM_INSTRUCT`, `VOX_ROOM_LANGUAGE` | `aiden`, none, `English` | Qwen3 voice, delivery and language |
| `VOX_ROOM_TTS_VOICES` | asked of the server | the voices a [puppet](#puppets) may be built on, comma-separated. Normally not set: the room asks `/v1/audio/voices` as it opens and keeps the server's own voices, leaving out the clones uploaded to it. Set it for a server that won't say — and spell the names the way its `/v1/audio/speech` takes them (`uncle_fu`, not "Uncle Fu") |
| `VOX_ROOM_LANGUAGES` | | other languages the door offers, comma-separated and spelled out in English (`Russian`). The AI answers in each one somebody is listening in; needs `--tts qwen3` |
| `VOX_ROOM_TRANSLATE` | `openai` | who translates: `openai` asks the reply model, `local` asks Marian models on the CPU (see below) |
| `VOX_ROOM_INTERPRET` | `true` | say what people say to the people who don't speak it, with the voices being interpreted ducked under it. `false` to translate only the AI and what is typed |
| `VOX_ROOM_INTERPRETER_VOICE` | `ryan` | the interpreters' Qwen3 voice. Not the AI's, on purpose: the one thing to be able to tell by ear is which is which |
| `VOX_ROOM_KOKORO_VOICE` | `am_michael` | Kokoro voice |
| `VOX_ROOM_MCP` | | MCP servers the AI may use, `NAME=URL[=TOOL,TOOL,...]`, semicolon-separated. See [Faces](#faces) |
| `VOX_ROOM_MCP_KEY` | | their keys, `NAME=KEY`, semicolon-separated — or the key on its own when there is exactly one server. Never reaches the model |
| `VOX_SCRIBE_MODELS` | scribe's models dir | speech-to-text models |

## Testing

`cargo test -p vox_room` covers the mixer, turn taking, barge-in,
recording somebody reading aloud, and the puppets — including a whole
show played out against a real room and a real mixer on a real clock,
which is the only way to test that a pause is a pause and that an
overlap lands where the script said. Two more talk to real servers and
are not run by default:

```bash
VOX_ROOM_TTS_URL=… cargo test -p vox_room -- --ignored speaks_two
VOX_ROOM_TTS_CLONE_URL=… cargo test -p vox_room -- --ignored clones
```

The second keeps a voice under a name, lists it, speaks a sentence in it
and deletes it again.

`crates/vox_room/e2e/run.sh` is everything that needs a running room and
nothing of yours. It starts a room of its own on a scratch database,
mints the invite links each suite wants (a link is spent by **Join
Audio**, so every run needs fresh ones), runs the suite and stops the
room again — your own database is never touched:

```bash
cargo build -p vox_room --release
cd crates/vox_room/e2e && npm i playwright   # not a dependency of anything built here
CHROME=$(which chromium) ./run.sh            # or: ./run.sh rooms bots
```

| suite | |
|---|---|
| `rooms` | several rooms on one socket: where a tab lands, who may go where, a room of your own admitting one person, and the mix arriving across every move. No browser at all — two sockets are enough |
| `bots` | two AIs in one room: a fader and a Stops setting each, each answering to its own name -- and each person's own **BOTS** switch, which keeps the AIs out of what *they* say without touching anybody else, still answers when they call one by name, or doesn't, and follows them into another room |
| `door` | the door is a page with no room behind it and no socket open; **Join Audio** does nothing until you agree to be recorded, and the Language box is never blank |
| `roles` | the house and the people: making a room, renaming one under somebody standing in it, taking one away from under them — and the owner and the rest: who is shown the people and who is answered when they ask for them, a room given and taken away while somebody sits in the house, and an invite made from the page |
| `settings` | the footer: one width for the talk button whatever it says, the two half-height switches stacked beside it, sharing and the way out on the other side, and the cog opening a page with what each switch means when it is off |
| `ptt` | push to talk: the talk key reaching the microphone from wherever the focus was left -- a button you clicked, the history panel, a mute -- and not reaching it in a text box, a dropdown or a dialog; Escape giving it back from each of those without disturbing what is in them; binding another key, the three kinds of key that are refused and why, and a bound combination still closing the mic when the modifier is let go of first; and what holding it does to the room's own sound, which is nothing until you ask for it, and then going to nothing while the key is down, coming back only once the mic has closed, and changing under a hold that is already being held |
| `permalinks` | `/`, `/lobby` and `/room/<id>`; back and forward move between rooms, a pasted address opens that room, a stale one is put right |
| `conversations` | the room's history: naming one, reading an old one read-only without moving anybody else's screen, deleting one for everybody, and empty ones never being archived |
| `game` | a game played inside a room, and leaving it putting the room back where it was for everyone in it |
| `cards` | what a posted card looks like once it is on the screen: the table, the list and the code block a model wrote in markdown — and the script tag it wrote with them arriving as words rather than as anything at all |
| `avatars` | the square beside everything somebody says, and the pictures it is chosen from: a face where one was made and the same-sized tint-and-initials where none was, faces beside the names in the list of who is here, a picture card that arrived beside one that never did, a face clicked open onto who somebody is (and onto an AI that says it is not a person), the last faces worn offered back in Settings and an older one put on again, and taking your own off turning it back into initials in the conversation behind the page |
| `drop` | a picture dragged into the room and one pasted into it: what the room says while a file is hanging over it, the card that lands on the dropper's own screen and on somebody else's, the next one getting the next number, a picture bigger than its square being smoothed where a sprite is not, a paste that is text with pictures in it being left to the cursor, and a file that is not a picture being refused in the words of whichever way it arrived |
| `interpreter` | the interpreter appearing only while it has somebody to interpret for, and its level being each listener's own |

The four below want something of yours — a microphone, a model, a
voice — so they are run by hand.
`crates/vox_room/e2e/two-people.js` runs two headless Chromiums against a
running room, each with a WAV file as its mic, and prints what each one
hears and the conversation as it unfolds (see the top of the script).
`e2e/share-screen.js` is the same idea for desktop sharing: one shares,
the other watches, and it reads the pixels off each canvas to check the
picture really arrived — and that opening it was one person's doing
alone. `e2e/hear-out.js` needs no microphone at all: two tabs on
different languages, a typed question, and it reads the per-language row
off both screens to check that one language is shown heard out while the
other is still going, that both tabs are shown the same thing, and that
the row goes when the turn does. `e2e/interpret.js` is the other half:
one tab's mic is a WAV of somebody talking and the other is listening in
another language, and it checks that what was said reaches that screen in
that language, that there is a voice saying it, and that the speaker is
shown how far it has got.
