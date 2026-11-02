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
interrupted.

It runs on macOS and Linux (no PipeWire: all the audio is the browsers').

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
there is nobody there to take the turn from, it starts out answering
every pause rather than waiting to be named — which you can change, and
it will remember. It is called *You and Vox*, after whatever you called
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
another one there — repeat it, or comma-separate `VOX_ROOM_BOTS` — and
it is in every room, the house's and everybody's own. Each has its own
voice, its own prompt, and its own name to be called by; the first is
the one `--name`, `--voice` and `--prompt` describe.

```
vox_room --bot Sage:serena --bot Ada:aiden:/etc/vox_room/ada.md
```

What each of them is is its own:

- **Its own name.** Saying "Sage" gives Sage the turn and nobody else.
  Nobody in the room may call themselves after any of them.
- **Its own fader.** One strip each, under the room, so turning one
  down is turning *that one* down.
- **Its own Answers.** When named, when it fits, every pause — per bot
  and per room, so Sage can answer every pause in your own room while
  waiting to be named in the Lobby.
- **Its own prompt**, with `{name}` filled in as itself, so each is
  told who it is rather than told it is the first of them.

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
- **Answers: when named / when it fits / every pause**, under the AI's
  own strip: who gets the turn when the room goes
  quiet. It is that bot's setting in that room — the same AI answers
  differently in the Lobby and in your own room, and a second bot in
  the room carries its own. Shared by everyone in the room, like the
  faders beside it.
  - **When named** — the AI says nothing until someone says its name, so
    two people can talk without it butting in. The default.
  - **When it fits** — at every pause the AI is *given the turn* and
    decides for itself whether the moment is its own. It is shown the
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
  - **Every pause** — answers whenever the room goes quiet, no judgment
    involved. The cheapest, and what a game that deals the turns wants.
- **The back-and-forth**: saying its name doesn't buy you one answer, it
  starts a conversation. From then on the AI keeps the turn at every pause
  without being named again — the button moves to "When it fits" with a
  dot beside it, so it is still judging each pause rather than answering
  every one — until the room has been idle for 30 seconds
  (`--engaged-for-ms`), when it goes back to waiting for its name. You can
  also end it in words: "thanks", "goodbye", "that's all", or just turning
  to talk to someone else, and the AI stands down by itself with its
  `wait_until_named` tool. Only the mode you pick with the buttons is
  remembered; a back-and-forth is never stored, so a restart comes back to
  what you chose.
- **Interrupting**: start talking while it speaks. It drops in volume at
  once and stops for good once your speech is confirmed (300 ms of voice
  or the first recognised word), so a cough only dips it. **Stop** does
  the same with a click.
- **Typing instead of saying it**: there is a box at the foot of the
  conversation. What you send through it appears beside everything said,
  marked **typed** — the mark is the point, because a typed line has no
  recognition error in it. It is for everything speech-to-text is bound
  to spoil: a URL, a path, a command, an unusual name. The AI reads it
  like anything else, and naming the AI in one gets its attention the
  same way.

  **Typing never interrupts.** Starting to type doesn't dip the AI and
  sending doesn't cut it off, however much you send: only a voice does
  that. A message joins the conversation at the next pause, with the
  same beat as speech in which to be joined by more.

  Messages are flattened to one line and capped at 2000 characters.
  That isn't tidiness: the AI is shown the conversation as `Name: what
  they said` lines, so a message allowed to keep a line break of its own
  could put words in somebody else's mouth. Unicode's direction
  overrides and zero-width characters go the same way, for the same
  reason. Everything else survives exactly as typed, because the
  punctuation a careless filter eats is what a URL is made of.
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
- **Sharing a desktop**: **Share screen**, in the footer, offers the
  browser's own picker of a window or a screen. Everyone in the room
  gets a thumbnail of it under that person's name in the mixer, live;
  click one and it opens full size where the conversation was, with the
  mixer (and everyone else's thumbnails) still beside it. **That is
  yours alone** — opening a desktop, or closing it, changes nothing on
  anyone else's screen, and nothing about it is remembered. Several
  people can share at once; each gets a thumbnail. **Stop sharing**, the
  browser's own stop button, **Leave audio** and walking into another
  room all end it. No sound is
  taken from what is shared: the room's audio is the mixer's, and a
  shared tab's would come back as another voice to transcribe.
- **The footer** is what you reach for while talking, and nothing else:
  your microphone on the left, what you do with the room on the right.
  **Mute** (or **Hold to talk**) is one width whatever it says, so the
  button under your hand is the same button it was a moment ago and
  nothing beside it moves; then **Share screen**, **Leave audio**, and
  the cog. Anything the room has to tell you appears along it too.
- **Settings**, behind the cog on the far right: a page of its own with
  the things you set once -- **Your name**, **Push to talk**, **Your
  volume**, what is coming in, and which microphone the browser gave the
  room. Changing your name changes it everywhere, the conversation
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

  Taking a room away, or taking it away from one person, happens where
  they are sitting: anybody left in a room that has stopped being
  theirs is put back in the lobby and told so. A room nobody may enter
  is not a room to leave somebody in — it would be off every list, with
  no way out of it but the door.
- **Mixer**: a level and mute for each person and each AI, shared
  by the whole room, hanging off the room you are in in the list on the
  left. **Your volume**, in the settings, is only yours. Space mutes
  your mic, or with **Push to talk** on, your mic is open only while you
  hold Space (or the talk button): everything said in one hold is one
  entry, and the AI waits for you to let go however long you pause.

  **The space bar is the microphone's wherever you left the focus.** On
  a button you have just clicked -- a room in the list, a mute,
  somebody's shared screen -- it still talks, and it does not press that
  button again while you hold it. The exceptions are where Space is
  something else: a text box, a dropdown, and a dialog that is asking
  you something. A focused button is still worked by Enter.

  Mute, push to talk and the mic belong to the **tab**, not to you, so
  two of your own tabs can't argue over them: you are muted once every
  tab with its mic open says so, and a tab sitting at the door — or left
  open on another screen with push to talk on — has no say in what the
  one you are speaking into does.
- **New conversation** clears the screen and the AI's memory. Old
  conversations stay in the database.

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
While a game that sets one of the last three is being played the
**Answers** control is locked to it; leaving the game restores what you
chose. A game also records how many players it is for (`players_min` and
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
  it keeps playing by what the room can see.

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
| `VOX_ROOM_BOTS` | | more AIs in every room, `NAME[:VOICE[:PROMPT_FILE]]`, comma-separated. See [More than one AI in a room](#more-than-one-ai-in-a-room) |
| `VOX_ROOM_REPLY_AFTER_MS` | `1200` | quiet before what was said is handed over |
| `VOX_ROOM_CONFIRM_MS` | `300` | continuous speech that cuts the AI off |
| `VOX_ROOM_ENGAGED_FOR_MS` | `30000` | idle time that ends a back-and-forth and goes back to "when named" |
| `VOX_ROOM_HISTORY` | `60` | entries of the conversation sent to the model |
| `VOX_SCRIBE_LLM_URL`, `VOX_SCRIBE_LLM_MODEL`, `VOX_SCRIBE_LLM_KEY` | as for scribe and `agent` | the model |
| `VOX_ROOM_REASONING` | `low` | how hard it thinks: `none`, `low`, `medium`, `high` |
| `VOX_ROOM_LLM_API` | `auto` | which API: `auto`, `responses`, `chat` |
| `VOX_ROOM_PROMPT`, `VOX_ROOM_PROMPT_FILE` | a short one for spoken group chat | system prompt; `{name}` and `{people}` are filled in |
| `VOX_ROOM_TTS` | see above | `qwen3`, `kokoro` or `none` |
| `VOX_ROOM_TTS_URL` | `PI_TTS_URL` | Qwen3-TTS server (`/v1/audio/speech`) |
| `VOX_ROOM_VOICE`, `VOX_ROOM_INSTRUCT`, `VOX_ROOM_LANGUAGE` | `aiden`, none, `English` | Qwen3 voice, delivery and language |
| `VOX_ROOM_LANGUAGES` | | other languages the door offers, comma-separated and spelled out in English (`Russian`). The AI answers in each one somebody is listening in; needs `--tts qwen3` |
| `VOX_ROOM_TRANSLATE` | `openai` | who translates: `openai` asks the reply model, `local` asks Marian models on the CPU (see below) |
| `VOX_ROOM_INTERPRET` | `true` | say what people say to the people who don't speak it, with the voices being interpreted ducked under it. `false` to translate only the AI and what is typed |
| `VOX_ROOM_INTERPRETER_VOICE` | `ryan` | the interpreters' Qwen3 voice. Not the AI's, on purpose: the one thing to be able to tell by ear is which is which |
| `VOX_ROOM_KOKORO_VOICE` | `am_michael` | Kokoro voice |
| `VOX_SCRIBE_MODELS` | scribe's models dir | speech-to-text models |

## Testing

`cargo test -p vox_room` covers the mixer, turn taking and barge-in.

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
| `bots` | two AIs in one room: a fader and an Answers setting each, each answering to its own name, and one pause going to exactly one of them |
| `door` | the door is a page with no room behind it and no socket open; **Join Audio** does nothing until you agree to be recorded, and the Language box is never blank |
| `roles` | the house and the people: making a room, renaming one under somebody standing in it, taking one away from under them — and the owner and the rest: who is shown the people and who is answered when they ask for them, a room given and taken away while somebody sits in the house, and an invite made from the page |
| `settings` | the footer: one width for the talk button whatever it says, sharing and the way out on the other side, and the cog opening a page with everything that used to be strung along it |
| `ptt` | push to talk: the space bar reaching the microphone from wherever the focus was left -- a button you clicked, the history panel, a mute -- and not reaching it in a text box or a dialog |
| `permalinks` | `/`, `/lobby` and `/room/<id>`; back and forward move between rooms, a pasted address opens that room, a stale one is put right |
| `conversations` | the room's history: naming one, reading an old one read-only without moving anybody else's screen, deleting one for everybody, and empty ones never being archived |
| `game` | a game played inside a room, and leaving it putting the room back where it was for everyone in it |
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
