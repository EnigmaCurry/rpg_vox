# Changelog

Changes in each release of scribe, Scribe for Android, Scribe for iOS,
agent, Pi voice, Voice room and rpg_vox. New entries go under
**Unreleased**; `just publish` names that section after the version it
releases.

## Unreleased

### scribe, Scribe for Android

- **A language can now be read in yours even when it is written the
  same way.** French into English was refused outright, and so was
  Russian with Ukrainian, because a paragraph's language was worked out
  from its alphabet and nothing else -- the recognizer that hears 25
  languages never says which one it heard. The alphabet was never wrong
  about Cyrillic against Latin, and could say nothing at all about Latin
  against Latin, so the honest thing was to refuse the pair rather than
  send a paragraph to a model that would translate it out of a language
  it was not in and return confident nonsense.

  The alphabet still goes first, still costs nothing and is still never
  overruled when it answers. What it could not settle now goes to
  fastText's `lid.176`: 917 KB for 176 languages, pure Rust, asked only
  about the languages you turned on and only above nine-tenths
  certainty. Under that the paragraph stands as it was said, which is
  what used to happen to every one of them.

  Nine-tenths is measured and not a round number. Over Flores-200 and
  over real decodes from this repo's own recordings it is where the
  wrong answers stop: Czech against Slovak against Polish is 2.2% wrong
  with no floor and 0.04% at this one, and the one English paragraph
  that was called Spanish is called Spanish at every floor up to 0.8
  and refused at 0.9. `examples/langid_bench.rs` is what took the
  numbers and `just langid-bench` takes them again.

  For Scribe for Android, reading in English, that turns five offered
  languages -- never more than two of them on at once, one per alphabet
  -- into twenty-one, with no limit on how many. For `scribe` it means
  `--other-language` may share an alphabet with yours or with another of
  them; without the classifier the old refusal stands, rather than a
  guess.

### Scribe for Android

- **Models are a page of their own.** Downloading them was a section of
  the Languages page that had grown until it was most of it, and it
  could only ever offer what the language settings happened to want.
  **Settings → Models** is now every model: what each is for, how big
  it is, whether it is on the phone, and a button to fetch or delete
  it, with the total on disk at the top. A row says whether a model is
  in use, unused, or wanted by a chosen language and missing -- which
  is the state in which nothing is being read, and which previously
  looked like a recording that simply would not translate.

  The Languages page keeps the three things it is for: the language you
  read in, the languages to listen out for, and what a paragraph shows
  while its reading catches up. Nothing on it downloads anything, and
  the rule that turning on one language turned off whichever other one
  shared its alphabet is gone with the restriction behind it.

- **A reading says that it is a reading, and will show you the line it
  was made from.** A translated paragraph was the same black text as
  one somebody actually said, with nothing to tell them apart and no
  way to see what the translator had been given. It now carries a rule
  down its left edge -- the whole of the indicator, present on readings
  and on nothing else -- and over it the language it was heard in,
  **From English**.

  Tapping that opens the thread sentence by sentence: each sentence as
  it was said, faintly, with its reading on the line beneath it. Zipped
  rather than the reading followed by the paragraph, because checking a
  reading means holding one sentence against one sentence, and down a
  long paragraph two blocks are too far apart to do it. A sentence the
  translator hasn't reached stands alone; tapping a reading plays from
  where its own sentence was spoken, as before.

  One switch for the whole transcript and not one per paragraph: any
  tag throws it, they all show the same chevron, and one tap puts the
  thread back to the readings alone. Reading a translated thread
  against the original is a way of reading, not a note taken on one
  paragraph. A paragraph nothing has been read of stays closed however
  the switch is set -- there is nothing to compare it against yet, so
  its tag is a label with no chevron.

  Distinct from **Show what was said**, which is about the sentence a
  reading has not reached yet. This is about the ones it has. The
  switch is not stored: it is how the transcript is being read just
  now, and it starts closed.

### Voice room

- **A recording can be dropped into the room and a voice cloned from
  it.** Cloning needed somebody to sit at a microphone and read a
  passage, which is the right way to get a voice from a person in the
  room and no way at all to use fifteen seconds you already have. A
  dropped file now gets the next number in the conversation, plays
  there, and `save_voice` takes that number exactly as it takes a read
  one.

  The browser decodes it, which is the whole reason it works for mp3,
  m4a, ogg and a voice memo off a phone: it already has a decoder for
  everything it can play, and the room would need a library for each
  and would still be guessing at the rest. What arrives is a WAV, and
  what the room does with it is what a server must always do for
  itself -- check it, make it mono at the rate the clone server wants,
  bound it to the thirty seconds a recorded clip is bounded to. Then it
  **reads the clip with the same model the room's own last pass uses**,
  which is already loaded, so the clone server is told what the clip
  says instead of cloning blind -- it clones markedly better that way,
  and a dropped file has no transcript of its own.

  **It is not dressed up as a recording**, for the reason a picture
  somebody brought in is not a picture the AI made: they are different
  events. It is its own kind of entry, the card says it was brought in
  and names the file, and the AI is told plainly that nobody read this
  one here -- a model told only that there is a clip in the chat thanks
  somebody for reading a recording they already had. The clone server's
  own consent field records which of the two kinds a clip was, since it
  asks for one and that is what the house actually knows.

- **A room with a puppet in it is a playhouse, and the director cannot
  hear the audience.** A show used to be a thing the room did to itself
  for the length of a script: everybody silenced, everybody finishing
  their sentences, then back to normal. That is right for the three
  minutes a scene takes and wrong for everything around it. So a show
  is now *a puppet being in the room* and nothing else --
  `bring_on_puppets` opens the playhouse, the last `send_off_puppets`
  closes it -- and a script is something that happens inside a show
  rather than what a show is.

  An open playhouse has three parts, stacked above the conversation:
  the **stage** with the characters on it at twice the size they are in
  the conversation, the one speaking lit, and a log of the lines they
  are saying; the **director's box** where the AI running it sits; and
  the **audience**, which is the conversation and everybody watching.
  **Sit here** on a band moves you, and the one seat nobody may move is
  the director's.

  **The box and the stage cannot hear the audience.** One rule,
  refusing in one direction, and the point of the whole thing: a play
  with the audience shouting suggestions at the director is not a play,
  and a suggestion reaches the director the way it does in a real
  theatre -- somebody goes round to the box. It took three places
  agreeing: `hand_over` now keeps each speaker's words with the seat
  they were said from and hands each voice only the ones it can hear,
  `will_answer` won't promise a turn nobody is going to take, and the
  audience's lines are left out of the AI's transcript altogether
  rather than marked unheard, because a model shown a question and told
  to ignore it answers it. **This is a real change in what the room
  does**: the audience can no longer put a question to the puppets
  without going round to the box or up onto the stage.

  Going up **onto the stage** is joining the play. Whatever script was
  running stops where it is -- the puppets have to answer the person
  who is up there rather than say what was written before they were --
  and from then on it is an **adlib**, improvised a line at a time. The
  stage says which of the two it is.

  And **a play is not the room talking.** The lines are read on the
  stage and never enter the conversation; what the conversation keeps
  is one note at the curtain, folded up, with the whole script inside
  it. They were in the conversation to begin with, which made one
  conversation out of two: half an hour of somebody else's dialogue in
  the middle of yours, with no way to tell which was which.

  `put_on_show` also **ends the reply it was called in**. Asked for a
  play, the AI announced it, called the tool, and then said "that's it,
  how did that land?" -- before a word had been heard, since the play
  does not begin until it stops speaking. The tool's answer says to
  stop talking and so does the prompt, and the room then hands every
  tool result back with "carry on from there" on the end of it, which
  is the opposite instruction and the one nearer the model. So it is
  structural now, the way starting a game already was.

- **The AI can put puppets in the room and act out a scene with them.**
  A bot has a brain of its own, and two of them in a room are two
  models talking to each other -- a request each per line, each
  deciding for itself what to say next. That is the point of them, and
  it is also why they cannot act out a scene: neither knows what the
  other is about to say, so every line waits for a model to finish
  thinking and an interjection lands wherever the round trip put it.

  A **puppet** is the other half of that. It has a name, a face, a
  voice and a fader of its own -- to anybody in the room it is another
  voice in the room -- and nothing behind it at all. What it says is
  the first AI's, written in advance and said through it. Nothing ever
  gives a puppet the turn; saying a puppet's name gives the turn to the
  AI that holds it, which is how one left standing after a show takes a
  question.

  So `put_on_show` takes a whole script in one call and the room
  **renders it ahead of itself** -- a line or two in front of the one
  being heard, so the audio for line four is made while line three is
  playing. The lines then land exactly where the script put them: a
  beat of silence, an answer on the last word, or `over`, which brings
  a line in a given number of seconds before the previous one ends so
  that both are heard at once. The timing is the whole reason to do
  this; a play made up a line at a time is not worth the puppets.

  **Every line is spoken in full, and an interruption is written rather
  than performed.** A line that gets cut off is written as far as the
  character gets, ending in a dash -- "But I only wanted--" -- and the
  interrupting line comes in over its tail. The first cut of this
  worked the other way: it synthesized the whole line and then cut the
  audio off with the same machinery a person talking over a bot uses.
  That was wrong twice over -- the cut landed where the wall clock said
  rather than where the writing meant, and most of a line the speech
  server had been paid for was thrown away -- and it was also wrong in
  principle. A playwright knows the sentence breaks after "wanted";
  nothing else does.

  `pause` is now measured from the moment the line before it was heard
  *out*, which is what it always claimed to be. It was measured from
  the moment that line was *pushed*, so for any line longer than the
  pause it had already elapsed by the time the line drained, and every
  line ran back to back whatever the script asked for.

  **It is a conversation first.** The AI is told to ask who is in it,
  from the roster or made now, and what it is about, before it writes
  anything. `make_puppet` keeps a character under a name with a line of
  delivery notes -- written as instructions to an actor, *"gruff and
  slow, an old soldier who has told this story before"* -- which is
  what makes a puppet sound like itself rather than like the AI, and
  which is the usual answer in a house where nobody has recorded a
  voice: the notes go to Qwen3-TTS as its `instructions` over whichever
  of the server's voices the character was built on. Where somebody has
  read for a cloned voice, a puppet can have that instead. It wears a
  face the way everybody in the room does, by `set_avatar` and
  `set_bio` on its name, and the name, the voice, the face and the bio
  are the house's from then on: a character made one evening can be
  cast again months later without being made again.

  Everything a puppet's line *is* on screen -- the entry with its name
  on it, the words appearing as the audio plays, the dash where it was
  cut, the line in the history afterwards -- is the room's own reply
  machinery, used exactly as a bot's reply uses it. A puppet is one of
  the room's voices and not a list of its own, which is why it gets a
  fader, a face and a place on the roster without any of that being
  written twice.

  **While a show plays, the room is set for it.** Every voice in the
  room goes to Answers: never and Stops: when finished as the curtain
  goes up, and back to exactly what it was when it comes down -- both
  *set*, visibly, on the same controls anybody could set them with,
  because a scripted exchange that stopped dead when somebody cleared
  their throat is not a play and a director that broke off mid-scene to
  answer a question is not one either. A show binds every bot in the
  room the way a game's reply mode does. The settings are written down
  as well as set, so a server that stops in the middle of the second
  act does not come back up with its AI silenced for good. **Stop**
  brings the curtain down: that button is never a game's or a show's to
  take.

  When it ends the AI gets the turn, with a card saying what played and
  who is still standing there, and the room is its own again -- it
  answers and it can be interrupted. The puppets can be sent away with
  `send_off_puppets`, or left to take questions, which is `say_as`: one
  line as one of them, rendered first and queued second so two of them
  answering in turn have no hole between them.

  Needs a speech server. Without one there are no puppets at all and
  nothing in the prompt mentions them -- a puppet with no voice is a
  name on a fader.

  The room **asks the speech server which voices it has** (`GET
  /v1/audio/voices`) as it opens, and keeps its own voices rather than
  the clones uploaded to it. That is not where this started: it held a
  list written down from the names a picker shows, and the first puppet
  ever asked to speak said nothing at all, because the voice everybody
  calls "Uncle Fu" is `uncle_fu` to the endpoint and the server
  answered HTTP 400 in the middle of the line. A preset asked for by
  the name anybody says it by is now matched to the server's own
  spelling and kept that way, and `VOX_ROOM_TTS_VOICES` is left as an
  override for a server that will not say. A voice that fails anyway
  reports **what the server said** into the room rather than "its voice
  said nothing at all", because the one who can fix a puppet built on a
  voice that does not exist is the AI that built it. See
  **[Puppets](ROOM.md#puppets)**.

- **The AI can clone somebody's voice from a few seconds of them
  reading aloud, and be asked to speak in it.** There is a second
  Qwen3-TTS server in `qwen3-tts/` that has always been able to do
  this -- the Base model, which takes a reference clip where the preset
  one takes a speaker's name -- and nothing in the room could reach it.
  What was missing was never the cloning. It was the recording: a few
  seconds of one person's voice, clean, with a transcript, taken
  without anybody being recorded unawares.

  The room already had all of it. The mic frames it transcribes are the
  frames a clip is made of, the transcription is the `ref_text` the
  server clones better with, and cards are how the room puts words on
  one person's screen. So `take_voice_sample` puts a passage on their
  screen, and a card in the conversation with a red dot on it saying
  the room is listening. **No microphone here is recorded without a
  light on it**, and the light is in the conversation where the room
  can see it rather than on the screen of the one person being
  recorded.

  The clip starts on their first word -- the room keeps the moment
  before it, so nothing is clipped -- and ends when they stop. Two
  words and a pause is forgotten and the room keeps listening: *"go on
  then"* is not a reading. Nobody reading at all is given up on from
  the clock rather than off their audio, because somebody who shut the
  tab sends none.

  **What they are reading is a clip and not a turn.** The room does not
  hand the AI a passage it wrote itself as something said to it, even
  where it answers every pause: `hand_over` waits for the reading, and
  what the AI is given afterwards is the pause with the card already in
  front of it -- how long the clip is and what they said. A bot that
  speaks only when it is named is given the turn outright, since nobody
  says an AI's name while reading at a microphone.

  Then `save_voice` keeps it under the name they asked for, and it is
  the house's: `list_voices` says what there is and asks the server
  whether it still has each one, `speak_as` wears one from the next
  sentence on, `forget_voice` deletes one for good. A voice is written
  down and which voice a bot is wearing is not -- that is this
  afternoon's, so a restart is every AI in its own voice again -- and a
  voice deleted from under a bot that is wearing it leaves it sounding
  like itself rather than silent.

  `VOX_ROOM_TTS_CLONE_URL` is the whole of the configuration. Without
  it the AI has none of these tools and nothing in its prompt mentions
  them.

- **A face can be drawn in more than one look now, and which one is
  yours to choose.** The picture server the room is pointed at grew a
  second style -- a 768-pixel cel-shaded portrait with ink lineart,
  beside the 64-pixel sprite it had always drawn -- and a room that
  had quietly been a pixel-art room found three things in its way.

  The look is now part of the interview. The AI is told that where the
  tools can draw in more than one, which look is the person's to pick
  and not its to default to: find out what there is, say what each one
  is in a word or two, and ask before anything is made. It belongs in
  the interview rather than after it because it is the one thing about
  a picture that a later change cannot give you -- a tool that changes
  one keeps the look it was drawn in, which is what makes it a change
  rather than a fresh start -- so changing your mind costs a whole new
  picture and another wait.

  **Not one style is named anywhere in the room.** Not in the prompt,
  not in the code, not in the browser. What looks there are is the
  server's to publish, and a room that listed them would be wrong the
  next time whoever runs that server adds one, with the AI
  confidently offering a look that is no longer there. So the room
  lends `list_styles` as a third tool -- it generates nothing, costs
  no GPU time, and is how the AI finds out what it is offering you --
  and says only the part it knows, which is that the choice is yours
  and is made up front. A call that only asks what there is gets no
  card: asking a question in the middle of an interview is not a
  picture being made, and a spinner for it would promise one nobody
  is making.

  **The most a face may be is 1 MB, up from 512 KB.** The old number
  was set against a sprite, which is a handful of kilobytes, and the
  headroom it looked like it had was gone against a full-resolution
  style: the busiest thing that server will draw, 1024 square and
  quantized to 24 colours, measures about 420 KB. A face the room
  refuses *after* somebody has looked at it and said yes is the one
  failure here that cannot be explained to them. It costs little --
  every face is fetched once and cached under a version that never
  changes meaning.

  **And the row of faces you have worn is drawn the right way round.**
  Every other square in the room already asked the picture which way
  it was being scaled -- blown up past its own pixels wants nearest
  neighbour, shrunk wants smoothing -- but Settings magnified
  regardless. A 768-pixel portrait in a 56-pixel thumbnail throws away
  thirteen pixels in fourteen that way, and what is left is noise
  rather than a face. It asks now, per picture, because that row holds
  both kinds at once: a sprite dropped in at its own 64 pixels is
  blown up, and anything a picture server made is shrunk. That bug was
  there before this style was, for a photograph dropped in and for a
  sprite the server handed back magnified; a row of ten thumbnails is
  just where it finally showed.

- **A picture can be dropped into the room, and the AI can be asked
  for it as your face.** Drag one onto the room and let go of it
  anywhere, or paste one with Ctrl+V -- a screenshot is on the
  clipboard as a file, so the two are one act with one path behind
  them. A paste that is text *with* pictures in it is left to whatever
  has the cursor: that one is meant for the box you are typing in, and
  a screenshot carries no text with it, which is how they are told
  apart. Either way
  it becomes a card in the conversation with your name on it
  and the next number -- the same numbering the AI's own pictures use,
  because "the second one" is said out loud about whatever is on
  screen and nobody in the room counts the two apart. From then on it
  is a picture like any other, which is the whole of why it is worth
  having: `set_avatar` already takes a number, so *"make that my
  face"* needed nothing new to work.

  The AI is told it is there, whose it is, and what the file was
  called -- and told plainly that it did not make this one. It is
  shown no picture here, its own included; a model told only that a
  picture is in the chat takes credit for it, and then offers to
  change a thing it never made.

  The tab re-encodes what was dropped before any of it is sent:
  decoded by the browser, so whatever the browser reads is a picture
  here, scaled down to the size the room actually shows a picture at,
  and written out as a PNG, which is what everything here that holds
  one has always been. So a photograph straight off a phone comes in
  as a few tens of kilobytes instead of being refused for being what
  phones make. The server keeps nothing over 1 MB, which is also the
  most a face may be -- anything in the chat can be worn -- and checks
  the bytes and the size for itself, because a page is not where a
  limit lives. Anything that is not a picture is declined where it was
  dropped, with a word in the footer and nothing uploaded.

  The faces are no longer a thing only a picture server can offer.
  `set_avatar` and `set_bio` were gated on the house having one, on
  the reasoning that a tool with nothing to put is a tool to leave
  out -- which stopped being true the moment a picture could arrive
  without being drawn. The prompt split along the same line: what the
  room knows about faces is always there, and the interview, the cost
  and the iterating are added only where something can actually draw.

  **A picture is drawn the way it is being scaled**, which is a thing
  the room had not had to decide before. Faces here are pixel art, and
  everything that showed one asked for nearest-neighbour: blowing a
  128-pixel sprite up into the square is the whole point of the form.
  A photograph is the same question with the numbers the other way
  round -- it arrives bigger than anything that shows it, and nearest
  neighbour on the way *down* throws most of it away. So the picture
  is asked rather than the room guessing: wider than the square it is
  shown in means smoothing, narrower means pixel by pixel, answered
  when the browser has the image and in one place for the card, the
  face beside every line, and the card that opens when a face is
  clicked.

  It is its own kind of entry rather than a made picture with a
  person's name on it. The two are different events -- one is the AI
  having done what it said it would, the other is somebody bringing
  something in -- and they read differently on screen and to the
  model. What they share is the numbering, which is the half that
  matters out loud.

- **A picture is made where the room can watch, and a face is only set
  once somebody says yes.** The first version of this went from "make
  me an avatar" to a changed face in one unbroken motion: the AI
  generated a picture nobody had seen and put it on you. It worked,
  which was the problem -- there was no moment in it where anybody
  could say no.

  So a picture now goes into the conversation as a card of its own,
  numbered, the moment the AI starts drawing -- with a spinner on it
  until it lands. Generating one takes the better part of a minute,
  and the card is what tells the room that something is happening
  rather than that the AI has gone quiet.

  Then it is looked at and argued about. Another take is another card
  with the next number, and the earlier ones stay where they are, so
  going back to the second one is a thing somebody can ask for and the
  AI can do. `set_avatar` takes that number rather than "the one you
  just made", and the prompt tells it not to call it at all until
  somebody has said they are happy.

  Clicking any face -- in the conversation, anybody's -- opens who
  that person is: the picture at a size worth looking at, a few lines
  the AI wrote about them in the same interview, what the picture was
  asked for, and what the house knows (the language they read in,
  whether the room is theirs, whether they are here). `set_bio` writes
  those lines and can rewrite them without making a new picture. An
  AI's own face opens the same card, which says it is not a person.

  The pictures belong to the conversation and go when it does. A face
  does not: it is copied when it is set, so deleting the conversation
  you sat for leaves you looking the same. One that was still being
  drawn when the room stopped is marked as never having arrived on the
  way back up -- the call it was waiting on died with the process, and
  a spinner is a promise that something is still happening.

  A picture is nobody's until it is assigned. Any of them can go to
  anyone here -- a person, the AI itself, or another AI in the room --
  so one picture can be looked at and then given to whoever it suits,
  and one nobody wants is offered around rather than thrown away.

  **Settings keeps the last ten faces you have worn** and offers them
  back. Choosing one again costs nothing and needs no AI, so it is a
  row of thumbnails and a press rather than something to ask for out
  loud; taking your face off leaves it in that row, because that is
  not the same as never having had one. Faces are also beside every
  name in the list of who is here, not only in the conversation.

- **Everybody has a face now, and the AI can make you one.** Every line
  in the conversation carries a square beside it: a picture when one
  has been made, and otherwise the tint that name already has with the
  initials in it -- the same size and shape, so the column of names
  lines up whether or not anybody has sat through an interview.

  Making one is a conversation rather than a form. Ask out loud, and
  the AI asks who you are, what you are wearing, what sort of face,
  says what it is about to make, makes it and puts it on you; then you
  tell it what to change. It cannot see what it made -- it is told how
  many pictures it has and refers to one by number -- so the room is
  its eyes, and setting another replaces the first.

  The pictures come from an **MCP server**, which is new and is not
  only for pictures: `--mcp NAME=URL=TOOL,TOOL` lends the AI any
  streamable-HTTP MCP server's tools, under a prefix of their own. A
  ComfyUI pixel-art server is what it is run against:

  ```bash
  vox_room --room Lobby \
    --mcp 'art=https://comfyui-mcp.example.com/mcp=generate_pixel_art,refine_pixel_art' \
    --mcp-key "$ART_KEY"
  ```

  Two tools, because a change to a face is usually a change and not a
  fresh start: `refine_pixel_art` edits the picture already made --
  "give it a red hat" -- and keeps everything else as it was. The new
  one gets the next number and the earlier ones stay in the chat, so
  going back to one is still a thing to ask for.

  **A card remembers what the tool said when it made it.** Changing a
  picture means telling the tool which one, by a handle only the tool
  knows -- and the tool says it once, in the reply to the call that
  drew the thing. That reply does not survive the turn: the
  conversation a model is given is what was said out loud, and a tool
  result is not that. Nobody asks for the hat to be red in the same
  breath as "here it is", so by the time it was asked the handle was
  always gone, and the AI would say it did not have one. The tool's
  own words are kept against the card now and read back every turn
  after, verbatim -- the room does not know which server it is talking
  to, and what the tool said is what the tool will understand when it
  comes back. They are kept for the model and not for the people: a
  job id is not something anybody is going to read out, and the
  browser is never sent it.

  **The tool list after the URL is the point of the flag, not a
  convenience.** That server offers seven tools and a room that wants
  faces needs those two; the rest are five more ways for a voice
  answering "make me an avatar" to end up somewhere else -- uploading
  a file the AI does not have, re-posting a picture already in the
  chat, listing every job on the server including other rooms'. Which
  tools a room lends is a decision about the room, made once in the
  configuration, rather than one the model re-makes every turn.

  **The key never reaches the model.** Some servers take it as an
  argument rather than a header -- Retro Diffusion's tools want an
  `rd_api_key` -- and a model shown a field like that fills it in:
  with something it invented, or, worse, correctly, which would put
  the key into a transcript that is written down and shown on screen.
  So it is stripped out of every schema the AI is shown and filled
  back in when the call is made. A server that wants only the bearer
  header never has anything stripped.

  Nothing is said to the AI about what a picture costs, because the
  room cannot know: a server on the house's own machine costs GPU time
  and a hosted one bills per image. What it is told instead is the
  wait -- the whole room spends it listening to nothing -- which is
  why it says what it is about to make before it makes it, and makes
  one at a time.

  A face belongs to the person and not to the room or the
  conversation: it is kept against the name on their invite rather
  than the name the room calls them, so renaming yourself keeps it, it
  follows you between rooms, and it outlasts the conversation it was
  made in. **Settings -> Your face** takes yours off, and yours is the
  only one you can take off -- not even the owner can clear somebody
  else's.

- **A card is written in markdown, so an AI can show you a thing while
  it tells you.** Ask for the weather and the answer is a handful of
  figures: speech is bad at "nine degrees, fourteen degrees, two
  degrees" and a table is good at it. `post_card` already put text in
  the conversation for the whole room, beside the spoken reply and
  separate from it, but the body was rendered as plain text -- a table
  arrived as a row of pipes.

  So a card's body is markdown now: tables, bullet and numbered lists,
  headings, bold, code blocks, quotes, rules and links. The cards only
  one person can see are the same -- a hand is a list, a standing order
  has the important part in bold. It is the only thing in the room
  written in markdown, because it is the only thing here written to be
  looked at rather than heard, and the prompt asks for both halves: the
  short version in the AI's own voice, the detail on the card, and a
  word to say the card is there, since nobody hears one appear.

  **Nothing in a card can become markup.** The body is written by a
  model, so it is escaped before a single tag is built from it: every
  tag in the result was put there by the renderer, a `<script>` in a
  card is three words on the screen, and a `javascript:` link is not a
  link. No images, no raw HTML, nothing that fetches anything. The new
  `cards` suite asserts it against a card written to try it on.

  Every AI in the room can post one, including the ones that may do
  nothing else: writing something for the room to look at is saying it,
  the same thing they do with their voices. What stays the first AI's
  are the tools that change what somebody is *holding* or what the room
  is doing -- `send_card`, `take_back_card`, and the games.

- **Which rooms an AI is in is the owner's to say.** A second AI was
  in every room the moment the server started with one: the house's
  rooms, and everybody's own. A room of your own is the one place
  nobody else comes into, so an AI arriving in it uninvited is the
  room breaking its own rule -- *You and Vox* with a third voice in
  it.

  So the AIs are given rooms the way people are, with a tick per room
  on **People and rooms**, and a column for **rooms of their own**:
  one answer for all of them, because which AIs are in a room of your
  own is the house's business rather than a thing to arrange person by
  person. The first AI is in every room and its ticks are fixed -- it
  is what a room has, the way a room has a lobby, and the owner's row
  in the table above it is fixed for the same reason.

  A tick reaches the room at once: a fader appears or goes for
  everybody sitting in it, and an AI taken out of a room it was
  speaking in stops where it is. Nothing is torn down -- it keeps its
  thread, its Answers, its Stops and everything it has said there, so
  being given a room is a tick and not a restart.

  Houses that already have a second AI keep it where it already was:
  every AI is placed once, into the rooms people share and into
  nobody's own, and from then on the table is the answer. Taking one
  out of the lobby stays done.

- **And they can put something to each other.** Seeing each other was
  half of it: a voice could refer to what another had said, but only a
  person could ever cause one to speak, so "say hi to Vortex Prime"
  got a hello that Vortex Prime never answered.

  **A name is somebody being addressed, whichever mouth it came out
  of.** A bot that finishes a reply naming another voice in the room
  hands that voice the turn, which is the rule the room has always had
  about who is being talked to -- it had only ever been applied to
  people. `hand_to` does it deliberately as well, for a bot that wants
  the next turn to be somebody else's without naming them aloud. That
  is what makes a conversation of three rather than two people taking
  turns with one AI.

  A name in passing hands over too, and that is the design rather than
  a tolerated miss: the turn it hands over is one the other bot can
  give straight back, and a turn given back leaves nothing at all. The
  cost of reading a mention as an address is one quiet turn. The cost
  of not reading it that way is the hello nobody answered.

  What keeps it from being two AIs talking while the people watch is
  `--ai-turns` (default 2): replies they may give each other with
  nobody human saying anything in between, and **the count goes back
  to nothing the moment somebody speaks or types**. So the people in
  the conversation are the whole of what keeps it going; stop joining
  in and it stops too, within a turn or two. `--ai-turns 0` is AIs
  that only ever answer people.

  A bot is told when it is on the last of them, and is not offered
  `hand_to` at all then: it is asked to finish the thought and put it
  back to the people. A bot that discovered the limit by calling the
  tool and being refused would already have written a reply for a
  conversation about to stop dead -- "I'll let Cora take that" with
  nobody able to.

  Turns that are waiting to be given out now live in one queue in the
  room, which is also where a bot's "show me this silence again" ask
  went. They want the same thing of the room -- a moment that is
  nobody else's -- and differ only in what happens when the moment
  doesn't come: a silence that didn't happen is spent, while a voice
  waiting to be answered waits for the voice ahead of it. Anything
  waiting is dropped when a person speaks, when the room is stopped,
  and when a bot is silenced.

- **Two AIs in a room can see each other.** Each was shown the other's
  replies as its own -- every AI entry went to the model as a plain
  assistant turn with no name on it -- so Vox read what Cora had just
  said as something Vox said, and would repeat it, answer for it or
  argue with itself. Each bot is now shown the others the way it is
  shown a person: a line of the transcript with the name on it, its own
  words still its own. Cards and saved games are attributed the same
  way, so "you posted a card" is only ever true of the bot that posted
  it.

  Each is also told which of the names in the room are AI voices and
  which are people, and a bot judging a pause is told a pause may be
  another AI's rather than its own -- "Ryan is the only person here, so
  there is nobody else they could be talking to" stops being true the
  moment there is a second AI in the room. A silenced bot is left out
  of all of this: a pause can't be its, so nothing is left to it.

- **And what each of them may do is not the same.** The first AI is the
  one the room was built around -- the one `--name`, `--voice` and
  `--prompt` describe -- and it is the one that runs the room: cards,
  games, the web. Every other bot is a voice in it, with `list_people`
  and `look_up` and nothing else. A second AI dealing cards and
  starting games is two hands on the same room, and nobody asked the
  second one.

  It keeps `stay_quiet` and `wait_until_named`, which are not powers
  but how a turn is handed back: without them a bot cannot be put on
  "when it fits", and cannot get out of a back-and-forth it was pulled
  into. Its prompt loses the notes about the tools it hasn't got, and
  a tool it was never offered is refused if the model calls for it
  anyway.

- **An AI can be told to say nothing at all, and nothing overrides it.**
  **Answers** has a fourth setting, **never**, beside *when named*,
  *when it fits* and *every pause*. A bot set to it stays in the room
  and keeps its fader, its voice and everything it has said; it simply
  has no turn to be given. Saying its name doesn't give it one, a
  back-and-forth can't start, and a game that wants every pause
  answered doesn't get this bot -- a game entered with every AI
  silenced opens in the quiet it was asked for, and leaving the game
  finds it still silent. Picking it while one is talking stops it where
  it is. With two AIs in a room it is how to put one aside for a while
  without taking it out of the room.

- **And whether talking over an AI cuts it off is now a setting of its
  own: Stops.** **When interrupted** is what the room has always done
  and is still the default. **When finished** means it says the whole
  of its reply: a voice still ducks it, so you can be heard over it,
  but nothing cuts it off -- so a cough, a false start or a word to
  somebody else can't cost you the answer you just asked for. **Stop**
  still stops it; only the voice is disarmed, never the button.

  What you said over it is neither thrown away nor taken for a
  question. It is transcribed and on screen as always, it waits where
  it is while the reply plays out, and then the pause it made is handed
  to the AI to *judge* -- the judgment "when it fits" makes, even for a
  bot on "every pause", and it is told the words arrived over its own
  voice. So a throat-clear costs nothing and a follow-up asked while it
  was still talking gets its answer one turn later. It is the same
  model call that pause was going to cost, asked a different question.

  Both settings are per bot and per room, like the fader beside them,
  and **Stops** is one a game never takes: a game says when a bot
  answers, not how it is stopped.

- **The footer is the two ways of saying something, and nothing else.**
  **Share screen** and **Leave audio** are now **📺️** and **🚪**,
  over the people in the left column with the cog beside them — that is
  the column the room is in, and the footer is for the button you aim
  at without looking. What takes the room they left is the box you type
  in, which moves down out of the conversation: saying a thing and
  typing it are the same act done two ways, so they sit side by side,
  and the conversation keeps the rows.

- **A typed message can have more than one line.** The box is a
  textarea now: **Enter** sends, **Shift+Enter** starts a line, and it
  grows to hold what is in it up to half the window, after which it
  scrolls. Its prompt is "Type your message here ...".

  Messages used to be flattened to one line, because the model reads
  the conversation as `Name: what they said` lines and a break inside
  one of them would read as the start of somebody else's turn. The
  flattening has moved to the one place that was ever about: the
  transcript handed to the model. What is stored, shown and read by
  people keeps the shape it was typed in — runs of spaces still become
  one, at most one blank line in a row survives, and every other
  control character is still a space.

- **Push to talk is beside the button it changes.** It was on the
  settings page, a page away from the button whose job it decides. It
  is a tickbox in the footer next to **Mute** / **Hold to talk** now.
  The settings page keeps what you set once and don't watch: your name,
  the app's volume, and which microphone the browser handed over. The
  **Coming in** meter is gone — it never worked — and **Your volume**,
  which is this browser's and not the room's, is called **App volume**.

- **The room has no title bar.** It held the room's name and four
  buttons, and every one of them belonged somewhere else. The name is
  the heading of the room column; starting a new conversation and
  leaving a game are over the conversation, beside the way into the
  old ones; and **Stop** is in the footer with the rest of what you
  reach for while talking. The row they shared is a row of the
  conversation now.

- **The rooms are behind their own name.** They were a list down the
  left with the room you were in opened out into its strips, so the
  strips sat at whatever height the list left them and moved whenever
  anybody made a room. Which room you are in is the heading of the page
  now; press it and every room you may go to drops down **over** the
  strips, press one and the list shuts and you are there. A house can
  hold a great many rooms, so the list scrolls rather than running off
  the screen — and the strips underneath never move, because they are
  the one place to look for who is here and how loud they are, and a
  list that pushed them down the screen would make you find them again
  afterwards.

  On a narrow screen the heading is above the tabs rather than inside
  one: which room you are in is true of whichever area you are looking
  at. The tab that held the rooms holds who is in this one, so it is
  called **People**.

- **The room hands a browser nothing until Join Audio.** Opening an
  invite link used to write the token straight into a cookie and keep
  it for a day. Looking at a link is not using one: `/join/<token>` is
  served the app and nothing else now -- no cookie, no redirect, nothing
  written down -- and the token stays in the address bar, where whoever
  followed the link put it. The page holds it in memory and sends it
  with each of the three requests the door makes; **Join Audio** spends
  it for the one cookie this browser is ever given, and is the only
  thing that puts anything in a browser at all.

  So a link you opened and thought better of leaves no trace: close the
  tab and there is nothing to find. And a browser with no cookie and no
  link in the bar is one the room knows nothing about -- which is what
  being logged out is, and which it did not used to be: a day-old link
  cookie went on greeting you by whatever name it named, outranking
  your own cookie, and **Join Audio** would have spent somebody else's
  link and made the browser theirs. A link the *page* is holding now
  only speaks for a browser that has no name of its own; otherwise the
  door says plainly that it is carrying one, and logging out is what
  hands the room over to it.

  The token travels in a request body rather than a path or a query,
  because a body is the one part of the exchange an access log doesn't
  keep. The URL is the only copy there is.

- **Fixed: the door could greet you with somebody else's name.**
  The door answers with the identity of the one browser that asked --
  who it is, or whose invite link its page is holding -- and it said
  nothing at all about caching. A private window held an earlier test's
  answer in memory and greeted the next browsing session with the wrong
  account's name, on a fresh invite link that had set the right cookie.
  Every endpoint that answers one browser about itself now goes out
  `no-store` with `Vary: Cookie`, so neither the browser nor anything
  between it and the room can hand one person's answer to another.

- **The app's own files are cached the right way round.** The hashed
  files under `assets/` are named after their contents, so they are
  kept for a year and never asked about again; `index.html`, the one
  name that stays put, is revalidated every time against an `ETag`,
  because it is what says which hashed files the app is made of now.
  A missing file under `assets/` is a plain 404 as well: it used to be
  answered with the app itself, so a stale index asking for a bundle
  this build doesn't have got HTML where a module was expected, and the
  page came up blank with nothing to say why.

- **Your name in the room is yours to change, and it is the only name
  you can.** Whoever runs the room picks the name on your invite --
  it is what you are revoked by and what a room of your own is yours by,
  and it never moves. What everybody reads was settable at the door and
  never again; it is now **Settings → Your name**, from inside the
  room, with your microphone still running.

  Changing it changes it everywhere. The roster, the faders, the
  meters, your cards and every line you have ever said move with it, in
  the database as well as on screen: the room calls you one thing, not
  a different thing per paragraph, and an old conversation is read
  under the name on the roster now. The name you give up is free for
  somebody else, and taking it does not pick up what was said under it.

  A room socket is told whose it is when it opens, so each of your tabs
  comes back on a fresh connection under the new name -- the audio does
  not stop for it, and a desktop you are sharing keeps being shared.

- **A desktop you are sharing stops when you walk into another room.**
  It used to come along. A share is made *to a room*, and carrying it
  into the next one shows a room something nobody in it asked to see,
  with no moment for the person walking in to decide otherwise. It ends
  on the way out now -- the browser lets the desktop go, so the capture
  indicator goes with it -- and sharing where you have arrived is
  something you ask for again.

  A dropped connection is not walking out. A reconnecting socket used
  to be seated in the lobby and the tab had to ask to be moved back,
  which is a change of rooms: a moment of bad wifi silently moved you
  out of the room you were in, wrote the address bar to match, and now
  would have ended your share as well. The socket says which room it
  was sitting in, and the room is looked up as one of theirs like any
  other, so naming a room that isn't yours seats you in the lobby.

- **The box you type into is a fixture, not a bubble.** It was a
  rounded pill at the end of the conversation, stuck to the foot of the
  scroller and inside the conversation's own margins -- so it read as
  one more message, which is the one thing it isn't. It is a row of the
  room now: a bar across the full width of the conversation with a rule
  along its top, nothing round about it, and the caret where the names
  above it start. The whole bar lights up when it has the focus,
  because the whole bar is the field, and **Send** is a word at the end
  of it rather than a second pill. The header and footer have been
  brought in at the same time, and the rows they give up are rows of
  the conversation.

- **No pull-to-refresh.** A swipe down at the top of the room reloaded
  it on iOS Safari, which drops the audio, the socket and your place in
  the conversation -- for a page where there is nothing a reload would
  fetch. The document and every list in it keep their overscroll to
  themselves now, so running off the end of the conversation or the
  room list is the end of it rather than the start of a reload.
  `overscroll-behavior` is Safari 16 and later; an older one keeps the
  gesture.

- **The owner makes the rooms.** The house used to be the command
  line's: `--room` at startup, and a restart to add one. The rooms are
  rows now, made, renamed and taken away from the **People and rooms**
  page while the server runs and people are talking in them.
  `VOX_ROOM_ROOMS` still names the first rooms of a house that has
  none, and is ignored after that -- from the first start the house is
  the database's.

  A room is known by an **id of its own** rather than by its name, so
  the owner can call it something else and nothing follows the word:
  its conversations, its faders, its settings and who may go to it are
  all kept against the id. (A room that existed before this keeps its
  old name as its id, so nothing of its past had to be moved.)

  Taking a room away, or taking it away from one person, happens where
  they are sitting: anybody left in a room that has stopped being
  theirs is put back in the lobby and told why. A room nobody may enter
  is not a room to leave somebody in -- it would be off every list,
  with no way out of it but the door. The lobby is not the owner's to
  take away: it is where a tab lands and the one room nobody can be
  refused. What was said in a room that is taken away stays in the
  database rather than being destroyed by a click.

- **Somebody owns the room, and rooms are given out.** A house used to
  be flat: anybody with a link could walk into any room in it. Now one
  person **owns** it, made at the server with
  `vox_room invite-owner NAME` -- there is one, and the way to a
  different one is to uninvite the first. It is a command and not a
  button because a house whose owner can be changed from a browser is a
  house with a way in through its own front door.

  The owner invites everybody else and says who may go where, from a
  **People** page inside the room: a link to copy, a tick per room, a
  fresh link for one gone astray, and removing somebody -- link,
  browsers and rooms together. Nobody else is shown it, and nobody else
  is answered when they ask the server for it: every one of those
  endpoints asks who is asking and tells anybody else there is no such
  thing.

  Everybody has **the lobby** and **a room of their own**. Every other
  room is given out one person at a time, and until it is given it is
  neither on their list nor behind its address -- a room you may not go
  to and a room that isn't there get the same answer, because which of
  the two it is is not the asker's business. The owner has every room in
  the house; nobody has anybody else's own room, the owner included,
  because *Just you and Vox* is a promise the room makes on its face.

  Upgrading a room that was already running: everybody invited keeps
  every room they could already walk into, so the new rule takes nothing
  from anybody. A database made after it starts empty, as it should.

- **A footer you can reach for, and a settings page for the rest.** The
  foot of the room was a row of everything: the talk button, a push to
  talk box, the name of your microphone, a meter, a volume slider, then
  sharing and the way out. It is now your microphone on the left and
  what you do with the room on the right -- **Share screen**, **Leave
  audio**, and a cog -- and nothing in between. The talk button is one
  width whatever it says, so **Mute** and **Unmute** are the same shape
  and nothing beside it moves as you press it.

  The cog opens **Settings**, a page of its own with the things you set
  once: push to talk, your volume, what is coming in, and which
  microphone the browser gave the room. The room goes on running behind
  it -- you are still in the audio, and the space bar still mutes or
  talks from there. **Done** or Escape comes back.

- **Fixed: no Language box at the door until you had joined once.** The
  door is a page with no socket behind it, so it asks `/api/door` what
  the room offers -- but the answer carried the list of languages only
  for somebody arriving on an invite link. A browser the room already
  knew got a blank space where the box should be, until a socket from
  joining filled the list in, and the next reload lost it again. The
  door now tells either of them what the room offers, along with the
  language that person chose last time: the room has it against their
  invite, and it beats what this browser would have guessed from the
  system.

- **Fixed: the space bar stopped talking once you clicked anything.**
  Push to talk ignored Space whenever a button had the focus, and a
  button keeps the focus after you click it -- so opening somebody's
  shared screen, picking a room, or touching a mute left you holding a
  key that did nothing, and the way to get the microphone back was to
  know to click the background first. Space belongs to your microphone
  everywhere in the room now; it also no longer presses the focused
  button again while you hold it. Where Space is something else it
  still is: a text box, a dropdown, and a dialog that is asking you
  something. A focused button is worked by Enter.

- **More than one AI in a room, and one pause between them.**
  `--bot NAME[:VOICE[:PROMPT_FILE]]` (`VOX_ROOM_BOTS`) puts another AI
  in every room. Each has its own name to be called by, its own voice,
  its own prompt with `{name}` filled in as itself, its own fader under
  the room, and its own **Answers** setting per room -- so one of them
  can answer every pause in your own room while waiting to be named in
  the Lobby.

  **Who answers a pause** is settled by who speaks first. Every bot
  that could answer is given the turn at once and the first to actually
  say something keeps it; the rest are told to stop. Asking them one at
  a time would put a round trip of silence in the room for every bot
  with nothing to say, and letting them all finish would be two voices
  over each other. A bot cut off this way leaves nothing behind -- no
  bubble on anyone's screen, nothing in the conversation, and nothing
  for the model to read back later as a turn where somebody said
  nothing. A bot already speaking is never cut off by another starting:
  it is the first word that claims the pause, and it has already had
  it.

  Nothing inside the room assumes there is one AI any more. A card is
  signed by the bot that dealt it, a game's button by the bot that
  designed it, a failed reply names the bot whose turn it was, and
  nobody in the room may call themselves after any of them. The screen
  follows: each strip says what *that* bot is doing, **Stop** names
  whichever of them is talking, and the prose that used to say "the AI"
  says all their names.

- **One server, several rooms, and a room of your own.** A `vox_room`
  used to be a room. It is now a house: the rooms you name with
  `--room` (`VOX_ROOM_ROOMS`), which everyone shares, and on top of
  those one of your own for everybody -- made the first time you walk
  into it, admitting nobody else, and where the AI is yours alone.

  **Moving doesn't cost you the audio.** You call in once and stay
  called in. The socket, the mic frames going up and the mix coming
  back are all exactly where they were; what moves is which room's
  mixer the far end is plugged into, whose conversation your screen is
  showing and where your voice is being transcribed.

  The rooms are a list down the left, flush to the edge, one line each
  with a count of who is in it. **Your own room is first and stays
  first**, then the house's in the order they were named -- the one
  room that is always there and always the same is the one to put
  where the eye lands. The room you are **in** opens underneath itself
  into everybody there -- the faders and meters and mutes that used to be a
  panel on the right. The mixer is a branch of the room it mixes now,
  which is what it always was: a fader belongs to the room it is being
  heard in, and the room you are not in has none you could usefully
  touch. A list rather than a row of buttons because a server can hold
  a great many rooms, and a row of buttons cannot.

  **Every room has an address**, and the bar follows you from one to
  another: `/` is the room of your own (per person, so whoever opens it
  gets theirs), `/lobby` is the first room named with `--room`, and
  `/room/<id>` is every other one. Back and forward move between
  rooms, and a link pasted to somebody opens that room for them. The
  id is not the room's name, because a name is a thing people change
  their minds about and a link that changed with it is a link somebody
  has already sent on. An address that names no room quietly becomes
  the address of the room you ended up in.

  **A game is something a room does.** It is played in a room, by the
  people in it, and while it runs the game is what that room is --
  with the room's own name still in the heading and the game beside
  it, and the list of rooms saying which room has one on.

  And leaving a game now **puts the room back where it was**. What the
  room was talking about is put aside rather than thrown away, and
  *Leave the game* picks it up again -- every word of it, and every
  card anyone was holding -- for everyone in the room at once. It used
  to start a third empty conversation, so a game cost you the one you
  were having to get into it and another to get out. A game is an
  interruption; the room goes on afterwards. (*Restart game* is not
  finishing with one, so what it interrupted is still there.)

  **A room keeps the conversations it has had.** *New conversation*
  closed the one you were having and that was the end of it: it stayed
  in the database, unreachable. It is the room's history now, and
  **History** at the head of the conversation is the list — when each
  was, how much was said, and its opening line, newest first with the
  live one marked. Open one and you read it as it was, cards and
  translations and all, with no box to type into: the room has moved
  on from it, so read-only is what it is rather than a rule about it.

  **A conversation can be called something.** The name at the head of
  it is a box: type over it and that is what the room calls this
  conversation, for everyone in it and in the history from then on. It
  names whichever one you have open, an old one included.
  Left alone it is when it was — the time of the first thing said, or
  the day it was started if nothing has been — and emptying the box
  gives it back to the clock rather than leaving it blank. The room
  keeps the name and the numbers; how a date reads is each browser's
  own business, in its own language and timezone.

  What the AI is shown is who is in the room too: `list_people` says it
  lists who is in the room, and it was listing everybody invited with a
  flag beside them — which is how it came to deal a card to somebody
  two doors down. Addressing one to somebody who has just stepped out
  still works, and the card waits where they left it.

  A conversation nobody said anything in is not one the room had, and
  isn't on the list — pressing *New conversation* twice leaves nothing
  behind the second time. The one the room is having now is always
  there, empty or not, because that is where you are.

  Reading one is **yours alone**. Nobody else's screen moves, which is
  the only way it could work — a room where one person looking
  something up pulled everyone else out of the live conversation would
  be a room nobody dared look anything up in. Deleting one is the
  exception: it takes that conversation, everything said in it and
  every card dealt in it away from everybody, for good, and asks
  first.

  Conversations are written down with the room that had them, from the
  moment they start, which is what makes any of this possible. A
  database from before this keeps each room's *current* conversation;
  the ones before it can't be told apart by room, so they are left out
  of every room's history rather than guessed into one.

  **The door is a page, and coming in is something you agree to.** The
  Join Audio card used to be a sheet over a blurred room: the
  conversation was there to read, and the sockets were open, before
  anybody had joined anything. It is its own page now, with nothing
  behind it and nothing open — a tab at the door is not on a roster,
  not sent a conversation, and not counted as being anywhere, and who
  is in a room is no longer something the door tells you.

  And the card now says what coming in does: your microphone goes live
  to everyone else, what you say is transcribed and written into a
  conversation the room keeps, and that writing is sent on to the AI
  and to the translation and voice services the operator has
  configured. **Join Audio** does nothing until you agree. The answer
  is kept in the browser, so it is asked once, and unticking it takes
  it back.

  **The door's Language box always has a language in it.** It was blank
  for anybody the room already knew: the box's first option carried the
  empty string the room stores for its own language, and what the room
  answers with is the name. Every option is its own name now. The
  default is whatever the system is set to, when the room speaks it,
  and the room's own otherwise — a guess made once and written down in
  the browser, so it can never afterwards overrule a choice.

  **The interpreter is yours now, and only there when it is working.**
  It used to be one fader in the room's mixer, shown whenever the room
  was *configured* to interpret and shared by everyone in it -- so
  turning the voice down turned it down for people who needed it, and
  a room where everybody spoke the same language still carried the
  strip. An interpretation is heard by whoever is listening in that
  language and by nobody else, so it is theirs: the fader is per
  listener, the duck follows it per listener, and the strip appears
  only while somebody in the room is speaking a language you don't
  read. A room of your own never has one at all -- the AI is already
  answering in your language, and there is nobody in there to
  interpret to -- and no longer starts the threads and voices for one.

  **The top bar is the room you are in, and what you press.** "Vox is
  listening" has gone — what the AI is doing is on the AI's own strip
  in the room list. The heading is the room's name: the list says it
  too, but only on a screen wide enough to show the list, and on a
  narrow one it is behind a tab.

  **When the AI answers now lives under the AI**, in the room list, as
  that bot's setting in that room -- which is the shape a second bot in
  a room would need. And the Games menu has gone from the top bar: a
  game is announced in the conversation that designed it, and that card
  is where **Play** and **Peek** are, which is one place to look
  instead of two.

  The default room is called **Lobby**. That word already had a job
  here -- "the lobby" was the room being in no game -- so the button
  that leaves a game now says **Leave the game**, which is what it
  does and what it always did; a button reading "back to the lobby"
  beside a room called Lobby would be a button that lies.

  Each room is a whole room: its own conversation, its own faders, its
  own mode, its own games and cards. Walk out of one and back into it
  and it is as you left it. What is the person's rather than the
  room's -- the invite list, the saved games, your name, the language
  you hear -- is shared across all of them, so choosing a language at
  the door still follows you everywhere.

  A room of your own has a roster of one and a mixer with you and the
  AI on it. Nothing said in it reaches the house, and nothing said in
  the house reaches it. Because nobody is there to take the turn from,
  it starts out answering every pause instead of waiting to be named --
  having to say "Vox" to a room with only the two of you in it is a
  ceremony with no point to it. Change it and it remembers.

  **You are in a room or you are not.** The list under a room is who
  is there, and that is the whole of it: somebody invited but in
  another room is on that room's count, not greyed out as "away" on
  this one. The roster used to be everyone invited with a flag, which
  made "away" stand for two unrelated things — gone, and standing two
  doors down — and neither of them is a thing this room has any
  business claiming about a person. What they speak is still known
  wherever they are, so a line they said before they walked out is
  still read in your language.

  The strips lost their boxes and a third of their height along the
  way, since a column that has to hold the rooms as well cannot spend
  eighty pixels a person. Everything is still there: the meter, the
  fader, the mute. The decibel reading is the one thing that comes and
  goes — it appears while the fader is off unity, which is exactly when
  you are looking for it, and the fader gets its width back the rest of
  the time.

  Two other things came with all this. A desktop is shared with the
  room you are in and no other, and the share goes with you when you
  leave. And the clocks -- the 20 ms mixer frame and the 50 ms
  turn-taking tick -- are the server's now rather than each room's: a
  thread per room waking fifty times a second to find nobody in it is a
  poor way to hold an empty room open.

  Each room keeps its conversation, its mode and its faders in the
  database under its own name, so renaming a room starts it fresh and
  the old one's conversation waits there for the name to come back. An
  existing database has one room in it, and its things become the first
  `--room`'s the first time the new version starts.

- **"Just for you" is gone when there is nothing in it.** Half the
  window used to be given over to a column saying what might one day
  appear in it -- a secret role, a hand, what only you noticed -- and
  for most of a conversation that is all it ever said. Now the area is
  only there while you are holding a card: no cards, no column, and the
  conversation has the half back. The first one dealt opens it; the
  last one taken back closes it again. On a narrow screen the tab comes
  and goes with it, and if the last card is taken back while you are
  looking at that tab you are put back on the conversation rather than
  left on an empty one.

  Nothing about the cards themselves changed: they are still the AI's
  to give, rewrite and take back, and still on nobody else's screen.

## v0.2.0 (2026-11-01)

### scribe

- **A paragraph's sentences are translated in one call, not one call
  each.** `--translate local` ran the model once a sentence, so a
  paragraph that pass 2 hands over three sentences at a time cost three
  decodes. They go to CTranslate2 as one batch now, which is one decode
  over the three -- the same work the model does anyway, done once. The
  reading was keeping up on a desktop and falling behind on a phone,
  where the recognizers already hold the cores; this is the first thing
  to try there. `Translate::translate_all` is where a translator says it
  can take lines together: the default still asks line by line, which is
  what `--translate openai` wants (nothing to batch when the cost is the
  round trip, and it reads the lines before each one for agreement), and
  a run of new sentences is broken wherever one already read sits
  between them, so the context each line is given is what it would have
  been asked one at a time. Ten sentences of a real transcript, through
  the en-ru model: 1047 ms a call a sentence against 329 ms as one
  batch, with every reading identical -- which the test asserts, since
  batching is meant to change the cost and nothing else. Greedy search
  (`beam_size` 1) would take that to 234 ms and is not taken: it
  changes eight readings in ten, some for the better and some for the
  worse, which is a poor trade for a third off something that is
  already a third of what it was.

- **`--translate`: what was said in another language, read in yours.**
  Pass 2's recognizer hears twenty-five languages without being told
  which, but it never says which it heard — so the alphabet is read off
  the text, and a paragraph that comes back in Cyrillic is also put into
  English, a sentence at a time. What was said is never replaced: the
  reading goes under it, as a blockquote in the markdown, in its own
  colour in the TUI, and as `NAME.translated.srt` beside the subtitles of
  what was said. `--translate local` is Marian on the CPU with no
  network at all (about 50 ms a sentence, needs a build with the
  `translate-local` feature); `--translate openai` asks the `--llm`
  service and can be told to leave a command alone. `--native-language`
  is what you read in, `--other-language` what to listen out for. The
  alphabet is the whole of the detection and the limit is stated rather
  than papered over: a pair that shares one — German against English — is
  refused at startup, because guessing there means translating a line out
  of a language it was never in. See
  [translation](SCRIBE.md#translation-optional).

- **Every `just` recipe that builds scribe for this machine can
  translate locally.** `just scribe`, `just release-scribe` and
  `just install-scribe` all carry the `translate-local` feature now, so
  they are one build between them rather than three variants, and the
  binary in `~/.local/bin` is the one `just scribe-translate` runs
  rather than one that answers `--translate local` with "needs a build
  with the feature". `install-scribe` then has the installed binary
  fetch its own models, which lands them in the per-user directory an
  installed scribe resolves to rather than the checkout's.

- **`--translate local` downloads its model, like every other model.**
  It was the one thing in scribe you had to put in place by hand: a
  `just` recipe, a Python with `sentencepiece` in it, and the models
  copied to wherever the binary you were running resolves them. Now the
  first `--translate local` fetches the direction it is about to read
  (~80 MB) into the models directory it resolved, and says so while it
  does — `scribe download-models --translate` takes the same download in
  advance, `--both-ways` adds the reverse of each, and `just
  install-scribe` copies nothing anywhere any more.

  What stood in the way is that nobody publishes a `tokenizer.json` for
  opus-mt, and the SentencePiece library that reads the `.spm` they do
  publish cannot be in the same binary as sherpa-onnx — their protobufs
  are different versions of the same symbols. So the conversion is in
  Rust now (`vox_transcribe::spm`): a `.spm` is a protobuf, the pieces
  and their scores are two fields of it, and the pure-Rust `tokenizers`
  crate that reads the result can also write it. It produces the same
  bytes `tools/spm2tokenizer.py` did, which is checked rather than
  hoped for — that script keeps its place as the one that can compare
  against a real SentencePiece, and the bytes it wrote are fingerprinted
  in a test.

- **The release archives translate locally too, Windows included.**
  `just dist` builds scribe with `translate-local` on every target, so
  the Linux, macOS and Windows archives all answer `--translate local`
  rather than "needs a build with the feature"; iOS is the one left
  without. The models are not in the archive — 80 MB a direction — and
  do not need to be: an unpacked archive downloads the one it is asked
  for, as above. Three things had to agree before Windows would link it:
  the C runtime (sherpa-onnx's prebuilt archive and CTranslate2 are both
  built against the static one, while the C++ bridge between them took
  cc's default, and the linker refuses that pair), the CPU dispatch
  (CTranslate2 reads `__AVX__` to decide which copy of its kernels it is
  compiling, so the `-mavx` that libopus needs has to stay out of its
  C++, or the generic kernels the dispatcher falls back to are never
  built), and, for a cross-build only, what a library file is called:
  ct2rs looks for `lib*.a` whenever its build script is *running* on
  Unix, whatever it is building for, so `just win64` names CMake's
  `.lib` files twice over and runs the build script again.

- **Playback was in the left ear only.** On PipeWire, everything scribe
  plays — `--live`, `--play`, and `--chat` replies read aloud — came out
  of one speaker. The stream is fed mono samples and said so, which gets
  it a single port whose channel position is `MONO`; the session manager
  links ports by position, so `output_MONO` matched `playback_FL` and
  nothing at all was linked to `playback_FR`. Nor was there an upmix to
  be had, since the adapter had negotiated the one channel that was
  asked for. The stream now opens as stereo with its channels named and
  writes each sample to both, which is what the cpal backend (macOS,
  ALSA) always did with however many channels the device has. Exactly
  the capture-side bug fixed below, on the other side of the graph.

- **The translation keeps up with the speech instead of waiting for the
  paragraph.** A sentence is read as soon as it is finished, not when
  the paragraph it is in settles — which, at up to two hundred words a
  paragraph, could be a minute after the first thing somebody said. It
  is refined as the paragraph forms, too: pass 3 rewrites words at
  utterance boundaries and pass 4 rewrites whole paragraphs, and a
  sentence that changed is read again while every sentence that didn't
  is reused, because the sentence *is* the cache key. That is what makes
  it free: a fifteen-minute file now translated in thirteen passes over
  one paragraph takes the same wall clock, to within a second, and
  produces byte-identical output. The reading deliberately runs one
  sentence behind the words on screen — a translator given half a
  sentence returns half a thought — and shows a trailing `…` while it
  does. Nothing is written to disk half-translated: a paragraph never
  hardens on a reading that is short of its text.

- **`--pass1-language`: live text in the language being spoken.** Pass 1
  is the one pass that has to be told which language it is hearing —
  passes 2 and 3 work it out from the audio and the translation works it
  out from their text, but a streaming model has to be loaded before
  anything has been said. There has been a Russian pass-1 model all
  along; scribe could only *download* it, for `vox_room`, and always
  loaded the English one itself, so Russian speech showed ALL CAPS
  English nonsense until each utterance was decoded. Now
  `--pass1-language Russian` loads it (fetched on first use, ~28 MB) and
  the live text is Russian. It is a choice, not a detection: one engine
  loads one model, so a language this isn't set to still gets the
  nonsense — `--pass1 ellipsis` and `--no-streaming` are the ways to
  want neither. Pass-1 text is upper-cased for the screen now rather
  than relying on the English model's own tokens being upper case, so
  "dim ALL CAPS means still settling" holds in both languages.

- **Words no longer come out of order.** A short utterance that the
  live pass never showed (too quick for it) only reached the transcript
  when its re-decode finished, and was added after whatever had been said
  since: a word or phrase from the start of a sentence could turn up at
  its end, or, on a file, paragraphs later. It now goes where it was said.

- **`--jsonl`: dictation over a pipe.** Another program (an editor, a
  chat client) can start `scribe --jsonl`, read what you say as JSON
  lines on stdout (`partial` text while it is still settling, `final`
  once a paragraph is done) and send `finish`, `cancel`, `delete_word`,
  `set_text` or `ping` on stdin. `finish` runs every pass on what was heard and sends the rest
  before exiting, `delete_word` takes back the last word said, even
  one a later pass is still rewriting, and `set_text` hands over text
  edited by hand for new speech to follow. With `--standby` it stays
  running between dictations, mic released, so the models load once:
  `start` opens the mic, `finish` / `cancel` release it, `pause` and
  `resume` release and reopen it mid-dictation, `reset` clears the
  text. `--auto-finish SECONDS` (or `start`'s `auto_finish_ms`)
  finishes a dictation by itself once you stop talking. stdout carries nothing else; logs
  stay on stderr. See
  the [reference](SCRIBE_REFERENCE.md#--jsonl-dictation-over-a-pipe).

- **Every channel of a multichannel input is heard now.** On PipeWire,
  capture asked for a single mono channel and let the graph sort it out;
  the session manager links ports by channel position, so a lone `MONO`
  port on a stereo source matched `FL` and left `FR` unlinked. Half the
  room was silently dropped — a stereo device transcribed only its left
  channel. The stream now opens in the source's own channel layout and
  mixes it down itself.

- **One channel per speaker**: `-d <device>:FL` transcribes a single
  channel of a multichannel input instead of all of them mixed, so two
  mics on one stereo receiver become two speakers, `-d rx:FL -d rx:FR`.
  `:FR`, `:L`, `:right` and `:ch3`… also work. `scribe devices` lists the
  channels of anything with more than one, and the `a` menu offers them
  as rows of their own.

- **`y` copies the whole transcript** in the recording TUI (and
  `--once`), unless a paragraph is selected with `↑`/`↓` — then it
  copies just that row. `End` clears the selection to get the whole
  text back. It used to copy the newest paragraph when nothing was
  selected.

- **A reading arrives with the utterance it is of, not the one after
  it.** A sentence counted as finished only when there was whitespace
  after its full stop. That is right for the paragraph as it stands --
  there the last sentence is where the speaker has got to, and
  translating it would translate half a thought -- and wrong for the
  text pass 2 has settled, where the speaker *ended* that sentence and
  the clip closed. Holding it back waited for the next utterance, which
  is a silence away rather than a sentence away: long gaps between
  readings, and on a phone held through a monologue it looked like
  translation only happened when the recording stopped. The live pass
  now reads the settled text up to its last finished sentence
  (`language::complete_settled`), where a full stop at the very end
  counts and only an unfinished tail is held. A test fails if the first
  reading arrives any later than the first utterance.

### Scribe for Android

- **The app itself is in English or Russian.** Every word it says --
  buttons, settings, dialogs, notifications, the voice keyboard, what
  TalkBack reads out -- was a literal in Kotlin, which is to say the
  app spoke one language and had no way to be told otherwise. They are
  resources now, in `values/strings.xml` with `values-ru/` beside it,
  and the app follows the phone's language list; on Android 13 and
  newer `locales_config.xml` also gets it a **Language** entry of its
  own in system Settings, so it can be read in Russian on an English
  phone. Which language the app is in and which language it
  *transcribes* are separate, and were worth keeping separate: a
  Russian reader transcribing English wants one of each. Three things
  stay in English on purpose -- the changelog, which is CHANGELOG.md as
  it was written; a model's directory name, which is what the release
  it came from is called; and the engine's own errors, which a bug
  report wants quoted. Nothing already recorded is rewritten: a
  paragraph's speaker is still stored as `Me` or `Phone` and a reading
  still remembers `heard_in` by the engine's English name, so switching
  the language changes the words on the screen and not a byte of the
  thread. Russian numbers properly, too -- "перевод отстаёт на 2
  абзаца" against "на 5 абзацев", which is four plural forms where
  English has two. `StringsTest` holds a translation to the same names,
  the same format arguments and enough plural forms for its language,
  and fails the build when one drifts; [ANDROID.md](ANDROID.md#translating)
  says what adding a third language costs, which is a file and a line
  of XML.

- **The status line says when the reading has fallen behind.** It used
  to degrade in silence: recording from another app, the words kept
  arriving and the readings quietly stopped keeping up, with nothing to
  say whether the translator was behind, broken or switched off.
  Android gives the app you are looking at the fast cores and an app in
  the background the slow ones — measured on a Pixel 6, that is pass 2
  running a third slower and the reading at about half the speed of the
  speech — so "Listening 4:21 · reading 3 paragraphs behind" is now part
  of the status line, counting the finished paragraphs still waiting.
  The paragraph being spoken is never one of them: a reading runs a
  sentence behind on purpose. See
  [ANDROID.md](ANDROID.md#recording) for what to do about it, which is
  mostly to leave Scribe on screen.

- **A reopened translated paragraph plays from the sentence you tap.**
  The reading was stored as one piece, so a thread opened again had
  nothing to say which sentence was which: every tap on it played from
  the start of the recording. The sentences are kept now, each with the
  one it was made from and when that one was spoken
  (`messages.lines`, the shape the engine hands them over in), so a
  reopened paragraph taps the way a live one does. Paragraphs stored
  before the column keep the whole reading against the whole
  paragraph -- the alignment they were saved without cannot be worked
  out after the fact.

- **What was said shows until its reading lands, in the language it was
  heard in.** A reading arrives a sentence behind the speaking, and the
  sentence being spoken used to be a faint `…` -- a paragraph being
  heard in another language was an ellipsis and nothing else until the
  translator had read its first sentence. The words themselves stand
  there now, faintly, in the language they were heard in, each giving
  way to its reading as that lands, so the screen shows that something
  is being heard and roughly how much. **Settings -> Languages -> While
  a reading catches up -> Show what was said** turns it off for the
  ellipsis again.
  A reading is still what a translated paragraph is, and still what
  Copy, Share and the notification say of one: this is what the place a
  reading has not taken yet looks like.

- **Settings -> Languages says it in lines rather than paragraphs.**
  The page had grown three paragraphs of explanation around four
  choices, and the last toggle's description was six lines long with
  the switch floating somewhere in the middle of it. It fits on one
  screen now, with a heading that says what each part is for -- **I
  read in** ("All text will be written and/or translated in English"),
  **Choose secondary languages (one per alphabet)**, **Manage
  dictionaries** for what is to be downloaded or deleted, and **While a
  reading catches up**. The alphabet rule is in the heading rather than
  repeated under every greyed-out language, a leftover dictionary is
  named in languages ("English -> Russian") rather than in the
  directory it is kept in ("en-ru"), and what the page used to spell
  out is in [ANDROID.md](ANDROID.md#languages), where there is room for
  it. About says what the app is in one line: live transcription,
  translation and dictation on the phone, with local models.

- **A new thread is named for when you spoke, not when you made it.**
  Making a thread and then waiting to press record left the title
  stamped with the wait rather than the recording. A new thread is now
  named with today's date alone, and gains the time when recording
  starts -- so long as the title is still the app's and nothing has been
  heard in the thread yet. The title says whose it is (`auto_title` on
  `conversations`, which the desktop ignores) instead of being
  recognised by its shape: renaming a thread hands it over for good, and
  reusing an untouched new thread rather than piling empty ones up no
  longer rests on a regular expression matching the date.

- **A long recording no longer fills memory with transcripts the screen
  has not caught up with.** Every paragraph event carries the whole
  paragraph, which for a paragraph minutes long is megabytes of JSON,
  and a recording with two sources resends each piece of it on every
  update. When the main thread fell behind, each `poll` still handed it
  one more copy, and the engine's queue kept every update in between:
  after a quarter of an hour the app stopped answering and then died
  trying to turn a 290 MB backlog into one Java string. A drain now
  collapses the updates of a paragraph to its latest state, as it always
  did for the level meter, and a poller waits for the main thread to
  finish the batch before rather than handing it another.

- **It reads another language in yours, on the phone.** The translation
  the desktop got is in the app, with CTranslate2 built for the NDK, so
  a paragraph heard in Russian also reads in English with nothing asked
  of a network. **Settings → Languages** has the three choices it takes:
  the language you *read in* (what every translation is read into), the
  language the *live text* is in, the languages to *listen out for* (a
  model each, 81 MB, downloaded there), and what the transcript *shows*
  of a paragraph that was read.

  The transcript is in the language you read and in no other. A
  paragraph heard in Russian is shown in English, a sentence at a time
  as each is read, and the Russian is not shown at all -- there is no
  setting for it, because a transcript you can read is the point of
  translating one. What was said is still what is kept: it is in the
  thread, it is what every later pass refines, and it is what each
  reading is made from again when one of those passes changes a word, so
  a sentence pass 3 rewrites is read again. Each line of a reading
  carries the sentence it was made from (`TranslatedLine`) to keep the
  two together through all of that. A sentence not yet read shows as an
  ellipsis -- the reading is a sentence behind the speaking on purpose --
  and tapping one of a translated paragraph's sentences plays from where
  that sentence was spoken, a translation having no word timings of its
  own. Copy, Share and the notification say what the screen says.

  What is shown as what was said is now what is *not known to be* in
  another alphabet, which is two fixes in one rule. A paragraph pass 1
  is still typing has no verdict at all, and a paragraph whose alphabet
  could not be read -- too few letters, or too much of a mixture, which
  `<unk>` and a stray word manage easily -- gets the same verdict as one
  in your own language: nothing to translate. Both were shown as what
  was said, which for Russian is Russian. The app asks the alphabet
  itself now, by the same rule and the same numbers the engine uses
  (`script`, a majority of at least eight letters), and shows what was
  said unless the answer is an alphabet you don't read.

  Text too short or too mixed to place is still shown -- "OK." is as
  likely as "Да.", and hiding every short utterance to catch the second
  would lose the first -- and the engine is told to assume the language
  pass 1 is listening in for exactly those, so most of them are read
  rather than left to that judgement. Only when the alphabet says
  nothing at all: a line that is readable and in none of the languages
  being listened out for is a line in your own, and sending that to a
  translator would get it back unchanged.

  The words that appear *as you speak* come from pass 1, the one pass
  that has to be told which language to listen in -- it decodes as the
  words are said, so its model is chosen before any of them have been,
  where passes 2 and 3 work the language out from the audio. Nobody is
  asked which it will be: pass 1 starts in the language you read in and
  follows the speaking, and an utterance that came back in another
  alphabet is enough for the next one to be listened to in that
  language. It is the alphabet pass 2 heard that decides, the same
  reading the translation itself goes by, and it changes between
  utterances and never within one -- a decode swapped mid-utterance
  would lose the words already on screen. The model for a language you
  turn on is downloaded with it (27 MB for Russian), and until it
  arrives the settings page and the recording status line say the live
  text cannot be in that language yet, rather than listening in the
  wrong one without a word about it.

  The languages offered are the ones that can be told apart. A
  paragraph's language is read off its alphabet, since the recognizer
  that hears 25 of them never says which it heard, so a language written
  in the alphabet you read could not be told from your own and two
  written in each other's could not be told apart: turning one on turns
  off whichever other one shares its alphabet. What was said is always
  what is stored, and the only text with word timings, so playback,
  editing and cutting stay with it — "The reading" hides it rather than
  replacing it. Readings are saved with the transcript, in two columns
  the desktop's chat database ignores, so a thread reopens with them.

- **The command-line bundle translates too.** `just android`, the
  `dist/android-arm64/` bundle for a shell on the phone, is built with
  `translate-local` like everything else, so `--translate local` works
  there as it does on the desktop. It wants `curl` to fetch the model,
  which an Android shell may not have; `adb push` a models directory, or
  point `VOX_SCRIBE_MODELS` at one, if it hasn't.

  Both Android builds now ship `libc++_shared.so`: CTranslate2 is the
  first C++ of ours in them, and the NDK links the shared C++ runtime
  where sherpa-onnx's prebuilt libraries carry a static one. Leaving it
  out is a crash at startup on the phone and nothing at all at build
  time, so the APK's libraries are now checked against what they say
  they need, the way the bundle's already were -- which is how this was
  caught without a device.

- **The app downloaded the wrong Parakeet.** It fetched
  `parakeet-tdt-0.6b-v2` while the library had moved to v3, so a fresh
  install downloaded 487 MB and then failed to load with the file it was
  looking for missing. One side of that rename had been left behind.

### Voice room

- **First release: a voice chat in the browser for a few people and an
  AI.** Everyone joins with an invite link and a headset, hears everyone
  else and the AI, and reads one live transcript of who said what. The
  server mixes on a 20 ms clock and encodes a separate mix-minus for
  each tab, so nobody hears themselves — Opus over one WebSocket,
  WebCodecs in the browser. Each person's mic feeds its own
  `vox_transcribe` engine, which is what lets a line be labelled with
  who said it. The AI answers through any OpenAI-compatible model and
  reads its replies aloud with Qwen3-TTS (Kokoro or silent as
  fallbacks). Talk over it and it drops in volume at once, then is cut
  once the speech is confirmed: the log keeps only what the mixer
  actually played, ending in a dash, and the model is told where it was
  interrupted. No PipeWire, so it runs on macOS and Linux. See
  [ROOM.md](ROOM.md).

- **Everyone hears the AI in their own language.** `--language-also
  Russian` puts a **Language** on the door, and the AI writes its reply
  once, in the room's own `--language`, while a separate voice per
  language translates and speaks it. Two people are in the same
  conversation, each hearing the AI in their own language, over one
  shared transcript; the people are always heard as they are, in their
  own voices. The choice follows your invite rather than your browser,
  so it is there on your phone too, and a language nobody has picked
  costs nothing, since the AI is only translated while somebody is
  listening in it. Needs `--tts qwen3` — Kokoro only speaks English.

  Three things are stated rather than papered over. **You hear your own
  language or nothing**: a translation has to be written before it can
  be spoken, so it starts about three seconds after the original and
  those seconds are silence rather than the opening of the reply in a
  language you didn't ask for (measured on a real room: the first
  English audio at 3.6 s, the same reply's first Russian at 6.3 s).
  **If you are listening in a translation you cannot talk the AI down**,
  because you would be stopping it in the middle of something you
  haven't heard yet — **Stop** still works, a click saying where to cut
  without guessing. And **you read what you heard**: replies and typed
  messages appear in your language with a **Show English** link under
  them for the words they were written in, one language at a time with
  the other a click away, and which one you are looking at is yours
  alone.

  The words arrive at the pace of the voice reading them *to you*. A
  translation is a different number of syllables from the words it
  translates, so `Reply` keeps a `Vec<Said>` per language and
  `Mixer::ai_progress` reports every queue rather than only the
  canonical one. The turn ends when the *last* language has been heard
  out, not the first — ending it with the English took the rest of the
  reply away from whoever was still listening to it — and a reply still
  being spoken carries a bar per language under it, `ENGLISH heard
  RUSSIAN 2s`, so each side of the room can see it is being waited for
  rather than that something has stalled. Each voice says for itself
  when it is done, since an empty queue cannot tell a voice that has
  finished from one whose next sentence is still being translated.
  There is a thirty-second backstop with a warning, for the one case it
  is there for: a voice thread dying would otherwise hold that turn open
  for ever, and with it every turn after it.

- **People are interpreted too, not only the AI.** What anyone says out
  loud is said again, by a separate voice, to everybody in the room who
  doesn't speak it — the way a booth at a conference does it. The
  per-listener mix became `(everyone - you) - (1 - env) * foreign + ai +
  interpreter`, so only the voices actually being interpreted duck; the
  ones you can already follow stay exactly where they were, nothing
  being said over those. Pass 2 is the trigger, which is the first point
  there is a finished sentence rather than a guess that is still
  changing, and puts the interpreter about half a second behind the end
  of an utterance (45 ms to translate on the CPU, ~390 ms to
  synthesize). Pass 3 rewrites clips afterwards and is deliberately
  never spoken again: by then the words have been heard, and nobody can
  unhear them. One thread per language, each with its own voice, so what
  was said is heard in the order it was said;
  `--interpreter-voice` defaults to `ryan` rather than the AI's `aiden`,
  because the one thing a listener has to be able to tell by ear is
  which of the two is talking.

  Every language goes through one `@interpreter` strip, and how far the
  floor drops under it is that fader: `0.2^gain`, a straight line in
  decibels at about 14 dB of duck per unit. At unity it is the duck that
  was always there; at the top the interpreted voices drop 28 dB; at
  nothing there is no duck at all, which is what makes pulling it down
  *off* rather than a hole in the room. Down means off down to the cost,
  since the interpreter asks the mixer before paying a voice for
  anything — but the queues still drain and the reading is untouched,
  because taking a person's transcript away because somebody else found
  the voice intrusive is not a trade one slider should make.
  `--interpret false` is still there for a room that wants none of it.

  An interpreter with more than eight seconds still to say stops paying
  for a voice and only writes: another sentence on the end of that queue
  would be heard later still, and the words have been on screen the
  whole time, so being read in the present beats being heard in the
  past. The decision is per utterance and per language, so a queue that
  drains goes back to speaking by itself. Nothing is dropped quietly any
  more either — a reader given nothing has no way of telling that
  anything was skipped — so a failed interpretation shows what there is
  to show: the translation when only the voice failed, and the line as
  it was actually said when the translating is what failed. What makes
  an interpreter fall behind is not the translating but having nowhere
  to put the audio; `the interpreter is behind` and `the interpreter
  cannot keep up` are those two limits from outside.

- **`--translate local`: the room's translation on this machine.**
  Marian opus-mt through CTranslate2, about 110 ms a line against the
  reply model's 1900 ms, and nothing anyone says leaves the process.
  Both translators sit behind one `Translate` trait, so the room holds
  an `Arc<dyn Translate>` and neither the voice threads nor the typists
  know which one they have. A Marian model cannot be *told* anything,
  which is the one real cost — no "copy this URL through as it stands" —
  so the trait carries `needs_backticks` and `translate::protect` lifts
  backticked spans and fences out of a line before the model sees it,
  putting them back after.

  The tokenizer is the awkward part: SentencePiece and sherpa-onnx each
  bundle protobuf-lite, and statically linked that is two copies of the
  same symbols. The image's way out is to not have SentencePiece at all
  — `tools/spm2tokenizer.py` converts `source.spm` to a pure-Rust
  `tokenizer.json` in a build stage that has Python for exactly that
  long, and refuses to write one that splits any of seven probe lines
  differently from the `.spm`, so a wrong conversion fails the build
  rather than the room.

- **The AI judges whether a pause is its turn.** "After every pause"
  became three modes. **When named** is the default and says nothing
  until the AI is named, so two people can talk without it butting in.
  **Every pause** answers whenever the room goes quiet, no judgment
  involved — the cheapest, and what a game that deals the turns wants.
  In the new middle one, **when it fits**, the room hands the AI the
  turn with a `Cue::Decide` and it answers or calls `stay_quiet`, so
  three people can talk with it in the room without it taking every gap
  between them. A turn it passes on leaves nothing behind at all: no
  bubble, no "thinking" flashing every 1.2 s. Passing with
  `ask_again_in_seconds` has the room show it the conversation again if
  the quiet actually lasts, once per silence so a quiet room can't loop.
  The decision is the same model that would answer, with a tool to
  decline, so a pause it takes costs no second round trip and only
  passes cost a prompt.

  What it is told is the whole of it, and getting that wrong is silent:
  the note used to presume the answer, so with one person in the room —
  nobody else they could be talking to — the AI stayed out of that
  conversation too, and the mode looked broken rather than wrong. It now
  carries what the room knows and the model cannot guess: who is here,
  and whether a back-and-forth is already on. There is a live test
  against the configured endpoint, off by default because it costs what
  that endpoint costs.

- **Games the AI designs with the players, and cards it deals.** In the
  lobby the model gets tools to list games, check a name is free, save
  one once the players have confirmed it, read and revise one nobody has
  played yet, and start or delete it by voice. A saved game is a card in
  the chat; entering starts a fresh conversation run by that prompt
  alone, with the AI opening the scene. The prompt never leaves the
  server: **Peek** fetches it from `/api/games/NAME/prompt` for the
  person who asked and never over the room's broadcast.

  The cards are information the AI chose to give one person, so they are
  the AI's to keep: there is no discard, and dealing a title someone
  already holds rewrites that card where it sits — a hand that changes
  as it is played, rather than five copies of "Your hand". `read_cards`
  gives them back in full, because a long game outruns the conversation
  the model can still see and a game master who has forgotten who the
  traitor is cannot run the game. `post_card` is the opposite: one
  written into the conversation where everyone reads it and it stays,
  for what the room should be looking at rather than holding in their
  heads. A posted card cannot be taken back — it is part of what the
  room has been told, like anything that was said.

  The AI's every word is read aloud, and the prompt used to teach it to
  say "check your private area", which does not mean in speech what it
  means on a screen. It has the words now — "Ryan, check your cards" —
  and a test keeps the other phrasing out.

- **Sharing a desktop.** **Share screen** puts a thumbnail in the mixer
  — 320 px wide, 2 frames a second, VP8 — which is what makes the mixer
  live rather than a list of names; the full size is only *encoded*
  while somebody has it open, the relay telling the sharer's browser
  whether anybody is looking. The server never decodes any of it. It
  forwards frames and keeps one rule — a viewer is sent no delta frames
  until a keyframe has reached them — which is what makes dropping safe:
  a viewer who falls behind is put back to the next keyframe instead of
  being shown a torn picture, and that is also how somebody arriving
  mid-stream fills in. It is a second WebSocket (`/screen.ws`), and the
  separate connection is the point, a burst of video in the audio
  socket's send queue being head-of-line blocking applied to speech.
  What it has not got is WebRTC's congestion control: the rates are
  fixed, so a still desktop costs little and a bad network degrades
  without a feedback loop to degrade *gracefully*. Needs WebCodecs
  video, so the button is only offered where the browser has it.

- **Typing instead of saying it.** A box at the foot of the
  conversation, for everything speech-to-text is bound to spoil — a URL,
  a path, a command, an unusual name. What you send appears beside
  everything said, marked **typed**, and the mark is the point, because a
  typed line has no recognition error in it. **Typing never
  interrupts**: starting to type doesn't dip the AI and sending doesn't
  cut it off, however much you send, since only a voice does that.
  Messages are flattened to one line and capped at 2000 characters, and
  that isn't tidiness — the AI is shown the conversation as `Name: what
  they said` lines, so a message allowed to keep a line break could put
  words in somebody else's mouth. Unicode's direction overrides and
  zero-width characters go the same way, for the same reason, and
  everything else survives exactly as typed, because the punctuation a
  careless filter eats is what a URL is made of.

- **An invite link works once, and the door is where it is spent.** The
  link used to *be* the credential: it was the cookie, reusable by
  anyone it was ever forwarded to, and `uninvite` left every browser
  already holding one no worse off than before. It is a one-time token
  now, traded for a cookie of the browser's own when **Join Audio** is
  pressed, in one `UPDATE ... WHERE used = 0` so two browsers racing the
  same link can't both win; `uninvite` takes both. Opening the link
  spends nothing — it goes to the door, which says *Hi, Mike* and offers
  the name the room will call them by. That name is what the transcript,
  the strips and the cards are keyed by, which is why the field is only
  on an unspent invite: changing it later would orphan everything
  addressed to them. **Log out**, at the foot of the card, hands this
  browser's cookie back, and only that session goes, so another device
  stays in. An existing database migrates in place: the old cookie *was*
  the invite token, so each becomes a session and every browser already
  in stays in.

- **A door in front of the room, and three areas instead of a sidebar.**
  Joining the audio was a button in the footer among the mixer controls,
  easy to miss and easy to sit behind; it is a card in front of the room
  now, which stays visible but inert until you have joined, so everyone
  in the room is in it by ear. The microphone, the speakers and push to
  talk are chosen there — the decisions that belong before you come in —
  and the card says who is already in. On a wide screen what is yours
  alone is on the left and the room's conversation on the right, the
  same size, with the mixer beside them; a narrow one shows one of the
  three at a time, with a switch above it that counts the cards you are
  holding. The games left the sidebar for a menu in the top bar, where
  picking one opens its card: who it is for, how many are here now, how
  it runs the room, whether it has been played, and **Play**, **Delete**
  and **Peek**.

- **Push to talk, and muted meaning not heard.** With push to talk on
  the mic stays muted except while Space or the talk button is held, and
  stays open 300 ms after release so the last word gets through; the
  setting is remembered per browser, and one hold is one entry, the room
  not handing over until it is let go. A mic muted from the bottom bar
  or the mixer is no longer transcribed or taken for talking over the
  AI, so the AI coming back through speakers can't interrupt it, and
  **No headset** turns on the browser's echo cancellation for people
  playing the room out loud.

- **`--llm-api` and `--reasoning`: it can think and use tools at once.**
  OpenAI's reasoning models refuse function tools on
  `/chat/completions` unless the thinking is off, and the room puts
  tools on every reply for the cards and the games — so every game was
  being run, and every pause judged, with reasoning switched off.
  `/responses` takes both, and one request loop now drives either API:
  the body, the streamed events, the way a tool call comes back and the
  way its answer goes in. `--llm-api auto` asks `/responses` first and
  remembers once per run if the endpoint hasn't got one, which is what a
  local llama.cpp or vLLM will say. The two things that were both called
  thinking are now apart: `--reasoning none|low|medium|high` is how hard
  it thinks, whichever API that means, and `--llm-api` is which API
  (`--think` stays as the old name for `--reasoning medium`, and
  `VOX_AGENT_THINK` is read by hand now, so an old `.env` setting it to
  `0` or `no` doesn't stop the room from starting).

- **In Docker.** `docker/` is a self-contained deployment behind
  Traefik, with the labels interpolated from `docker/.env` and the
  speech-to-text models downloaded into a volume on first start. It
  assumes nothing about where the Docker daemon is — everything is a
  named volume, so a remote context works the same as a local one.
  `just config` walks `.env.example` and asks about every key,
  `just install` builds and starts, and `room-invite` / `room-uninvite`
  / `room-people` run the room's CLI inside the container against the
  same SQLite file the running room has open. `docker/.env` is
  deliberately *not* the repository's own. See
  [ROOM_DOCKER.md](ROOM_DOCKER.md).

### Pi voice

- **First release: a voice conversation with the Pi coding agent.**
  `pi/scribe-voice.ts` drives `scribe --jsonl --standby` for what you
  say and `pi/pi-tts.py` for reading the replies aloud, paced to the
  voice and shown as they are spoken; talking over a reply cuts it off.
  `just pi` runs it and `just pi-tts-venv` sets up the voice. The voice
  is Qwen3-TTS through mlx-audio on a Mac, or, with `PI_TTS_URL`, a
  vllm-omni server (rpg_vox's qwen3-tts stack), which is what makes it
  work from Linux. See [PI.md](PI.md).

## v0.1.10 (2026-10-18)

### scribe

- **Windows (x86_64).** scribe and agent now build and run on Windows,
  and each release has a `.zip` of them for `x86_64-pc-windows-msvc`
  beside the macOS and Linux archives. It carries the two C runtime
  DLLs as well, so the folder it unpacks to needs nothing installed.
  Use it from Windows Terminal.
  - The mic, the speakers and the device list come from WASAPI, through
    cpal as on macOS.
  - **System audio without a virtual device**: an output device opened
    for input hands back what it is playing, so `-d loopback` (the
    default output) or `-d "<output device>"` transcribes a call, a
    video or anything else coming out of the speakers. `scribe devices`
    lists them and marks which ones work that way, and the TUI's `a`
    menu offers them. That is the job `--virtual-sink` does on PipeWire
    and `--app` on macOS, neither of which Windows has: WASAPI can't
    single out one app.
  - Models live in `%LOCALAPPDATA%\vox_scribe\models`, the clipboard
    is written through Win32 (`clip.exe` would mangle everything that
    isn't ASCII), and sherpa-onnx's C++ log lines are kept off the TUI
    the same way as on unix, by swapping descriptor 2.
  - Not available there: `--app`, `--virtual-sink`,
    `--tts speech-dispatcher` and `scribe stop-once`. Each says so
    rather than misbehaving. `--once` itself records, copies and exits
    as it does elsewhere; what it loses is being finished by a second
    press of the key that started it, which signals the first process.
- **`scribe download-models` unpacks the model archives itself** instead
  of running `tar`, so nothing has to be installed for it. It skips
  anything in them that isn't a plain file or a directory.

## v0.1.9 (2026-10-15)

### scribe

- **Sentence starts are capitalized.** The recognizers punctuate text
  they don't re-case, so a sentence break pass 3 found inside a
  re-decoded window came out as "... in Hell. the surrounding land ...".
  The first letter after a `.`, `!` or `?` is now capitalized in passes
  2 and 3 — but not after `3.5`, `etc.` or a trailing-off `...` — and
  pass 3 no longer drops the capital from a clip that opens a sentence
  its window began on. rpg_vox's transcription does the same.

- **`--paragraph-gap SECONDS`** (env `VOX_SCRIBE_PARAGRAPH_GAP`) sets the
  quiet that ends a paragraph in auto mode, until now a fixed 2 s. It is
  the same wait a paragraph takes to settle and be written, manual ones
  included, so a shorter gap also gets text into the `-o` file sooner.
  `--chat` still starts from 0.3 s; passing the option overrides that
  too.
- **`--paragraph-soft-words` and `--paragraph-hard-words`** (env
  `VOX_SCRIBE_PARAGRAPH_SOFT_WORDS`, `VOX_SCRIBE_PARAGRAPH_HARD_WORDS`)
  set the word counts that end a paragraph, until now a fixed 100 at the
  next pause and 200 regardless. `0` turns a cap off, as `--chat` does
  with both.
- **A word cap now holds while the re-decode lags.** A clip joins its
  paragraph as soon as the live pass opens it, but the caps were only
  checked when pass 2 landed, so a paragraph swallowed every utterance
  the decoder was behind on — every utterance of an `--input` file,
  which is pushed faster than real time, leaving one 103-word paragraph
  where a 15-word cap was asked for. rpg_vox's transcription and Scribe
  for Android get the same fix.

## v0.1.8 (2026-10-15)

### scribe

- **speech-dispatcher reads `--chat` replies**, in whatever voices the
  desktop's own speech daemon is set up with, so a machine where speech
  already works chats without the 350 MB Kokoro download. `--tts`
  (env `VOX_SCRIBE_TTS`) picks the engine: the default asks
  speech-dispatcher and falls back to Kokoro when it doesn't answer —
  and only asks, leaving a daemon that isn't running alone rather than
  starting one.
  - `scribe voices` lists the names `--voices` and `v` take for the
    engine that would speak; speech-dispatcher's are `module:voice`
    (`pocket-tts:vera`, `espeak-ng:English (America)+Alex`), for the
    module and language the daemon is set to.
  - Where the module reports index marks (espeak-ng does, pocket-tts
    doesn't) the reply's words light up as they are really spoken;
    where it doesn't they are spread over the reply by length, timed
    from the replies spoken so far.
  - Reading a transcript aloud (`-p NAME` with no `NAME.opus`) stays
    Kokoro's: that player seeks and changes speed through the samples,
    and speech-dispatcher plays its speech itself.
- **Speech no longer comes out choppy on PipeWire.** The playback
  stream filled each buffer the server handed it (12288 frames, 256 ms
  here) instead of the frames the graph asked for (one 256-frame
  quantum), so every cycle drained the 200 ms ring dry and padded the
  rest with silence: in a measured two seconds of audio, 560 ms came
  out as holes. It now fills `Buffer::requested()`, and the ring holds
  500 ms so the fallback for servers that ask for nothing in
  particular still fits a whole buffer. Affects everything scribe
  plays — Kokoro replies in `--chat`, `--play` (a recording or a
  transcript read aloud) and `--live`. The same mistake, and the same
  fix, as rpg_vox's source stream.
- **A chat needs a responder.** `--chat NAME` waits a few seconds for
  one to say it is listening and then exits with the commands that
  start one, rather than opening a conversation that would never be
  answered. `agent` and `scribe chat-echo` say so in `NAME.db` every
  couple of seconds; `--no-responder` opens the chat regardless, for a
  responder of your own.
- **Dictation deletes by the word, or by the character.** `backspace`
  takes the word before the cursor and `del` the word after it (where
  `del` used to only delete the selection); `alt+backspace` and
  `alt+del` take a single character, like a notepad. `backspace` no
  longer takes the whole sentence — that moved to `shift+backspace`
  (or `ctrl+backspace`), which only terminals speaking the kitty
  keyboard protocol can tell from a plain `backspace`.
- **`e` opens a dictation for typing**, beside the `tab` and `i` that
  already did.

### agent

- **It checks the model answers before taking the conversation.** One
  request at startup, so a wrong URL, a missing API key, a server that
  isn't running or a model it hasn't got exits 1 with the server's own
  message — instead of surfacing mid-conversation as "Sorry, I couldn't
  get an answer just now." Reasoning models that reject `max_tokens`
  are asked again with `max_completion_tokens`, then with neither.
- **It says it is listening** in the chat database every couple of
  seconds, which is what `scribe --chat` now waits for at startup.

## v0.1.7 (2026-10-13)

### scribe

- **PulseAudio systems.** On Linux without PipeWire running, scribe
  records from the mic and plays back through ALSA, which PulseAudio
  routes as usual. Capturing an app (`--app`) and `--virtual-sink`
  still need PipeWire. `VOX_AUDIO_BACKEND=alsa` or `pipewire` picks one
  without checking.

## v0.1.6 (2026-10-13)

### scribe

- **Dictation**, as in Scribe for Android: `scribe --dictate NAME`
  writes `NAME.md` by voice and keeps it, so you can come back and keep
  going.
  - The arrow keys or a click move the cursor where new speech goes
    (lowercased where it lands mid-sentence); a click below the text
    puts it at the end.
  - `enter` starts a paragraph at the cursor. `backspace` deletes the
    sentence before the cursor, the paragraph break right before it,
    or the selection if there is one. Select with `shift` and the
    arrows, or by dragging.
  - `tab` opens the text for typing by hand at the cursor; `esc` goes
    back to dictating there, listening again if you were.
  - `space` starts and stops listening, with no pause. `y` copies the
    selection, or the whole text.

## v0.1.5 (2026-10-12)

### scribe

- Builds without a deprecation warning on the newest Rust toolchains.

### Scribe for Android

- **App icon**: the rpg_vox logo (the hooded figure with the mic), in
  place of Android's default.

## v0.1.4 (2026-10-12)

### scribe

- **Pass 2 keeps up with background music.** The voice detector's
  threshold now rises with the background level (2.5× the quietest
  moment of the last 3 seconds), so pauses between your words count as
  silence even with music or a fan on. Before, steady background kept
  every utterance open until the 12 second cap, delaying the re-decode.
  Quiet rooms are unaffected.
- **Pass 2 doesn't wait on sound without words.** When the live
  transcription (pass 1) has heard no new word for 1.5 seconds, the
  utterance closes even if it's still loud, and pass 2 runs on it.
- **`--denoise` (experimental)** runs the DeepFilterNet 3 noise filter
  on the audio before transcribing it, to push background music down
  and keep the voice. `--denoise-limit DB` caps how much it removes.
  While it's on, `◆ denoise` shows beside the level meter (which then
  reads the filtered level).
- **`--pass1 off|text|ellipsis|hidden`** chooses how the live pass-1
  text is shown: as now, as a run of `…` that grows with each word
  heard, not at all while pass 1 still runs (`hidden`), or not at all
  with pass 1 skipped (`off`). Either way text appears after pass 2.
  The colour legend under the meter follows it.

### Scribe for Android

- The voice keyboard shares scribe's engine, so it gets the same two
  background-music fixes: pauses count as silence over steady
  background, and audio with no new words closes after 1.5 seconds.
- **Dictations**, a second kind of thread beside transcriptions, for
  writing by voice rather than recording in order. Each is saved, so
  you can come back and keep going.
  - Tap the text to move the purple cursor where new speech goes
    (lowercased where it lands mid-sentence); a tap below the text puts
    it at the end. It works on text from earlier sessions too.
  - ¶ starts a paragraph at the cursor. ⌫ deletes the sentence before
    the cursor, the paragraph break right before it, or the selection
    if there is one. Selected words can also be deleted from the
    selection menu, across paragraphs.
  - ⌨ opens your keyboard to edit the text by hand, starting at the
    cursor. Closing the keyboard goes back to dictating where you left
    off, listening again if you were. The controls hide while the
    keyboard is up, and the text keeps the cursor in view as it opens
    and closes.
  - Record and stop is all it needs; there's no pause. Leaving the app
    stops listening and saves.
  - The voice keyboard and dictations share the same editing code. A
    cursor move or ¶ made while the keyboard is paused now stays where
    it was made.
- **One list of threads.** The drawer lists dictations (✍️) and
  transcriptions (🎙️) together, most recently changed first. **+ New**
  asks which to start, Dictation first.
- **Start recording on New** (Settings › Recording, off by default): a
  new dictation or transcription starts listening as soon as it opens.
- **One row of controls on every screen**, under a short status line:
  four round icon buttons mirrored around a big one in the middle,
  whose ring shows the mic level while recording, or how far playback
  or a file's transcription has got.
  - Dictation: ¶, ⌫, dictate/stop, ⌨, Copy.
  - Transcription: source (Mic, Phone or Both), Clear, record/stop,
    pause, Copy.
  - Playback: stop, back 10 s, play/pause, ahead 10 s, Copy.
  - Share, Rename, Delete and the settings pages move to a ⋮ menu at
    the top (it replaces ⚙). The separate level bar, source picker,
    playback bar and Copy/Share links are gone.

## v0.1.3 (2026-10-11)

### Scribe for Android

- **Voice keyboard: Pause.** Pause stops transcribing without losing
  what you have; the mic stays open so Resume picks up at once.
- **Keeps going when hidden.** Switching apps no longer throws the
  dictation away: it keeps listening (unless paused), so you can switch
  to read something out, and the text is there when you come back.
- **Purple cursor.** It marks where new speech goes. Tap between words
  to move it; what you say next goes there. Capitals and full stops
  where speech lands mid-sentence are fixed up ("a Brown dog." becomes
  "a brown dog").
- **Delete and Edit** in the text selection menu. Edit pauses and
  retypes the selected words on a small built-in keyboard.
- **¶ button** for a paragraph break at the cursor; otherwise the text
  is one paragraph, with no automatic breaks.
- The transcript follows the cursor as it grows. While the speech models
  load, the keyboard shows "Loading models…".
- Buttons are ⌨ (back to your other keyboard, discarding the dictation),
  ¶, Pause and Done (formerly Insert), all the same width.

## v0.1.2 (2026-10-08)

### Scribe for Android

- **Keep audio.** A Recording setting saves each recording (.opus) with
  its transcript. Tap any word to play from there: the words light up as
  they're spoken and the transcript follows along.
- **Playback controls:** back and ahead 10 seconds, pause and resume
  (also by tapping the transcript), and stop. They're in the
  notification and on the lock screen too, and playback pauses for calls
  and when headphones are unplugged.
- A thread plays straight through all its recordings, with a play button
  above each one; long-press it to delete that recording.
- **Keep non-speech audio** (off by default): recordings in which no
  words were heard are kept and shown as "No words detected". With it
  off, they're deleted, and playback shortens quiet between words to one
  second.
- **Edit and Cut** in the text selection menu: retype a paragraph, or
  remove words along with their stretch of audio. Cut also works while
  recording.
- Paragraphs stay in the order they were said, even when one is
  finished after a later one.
- Transcribing a file shows how far it has got and the time left.
- Settings are split into Microphone, Recording, Keyboard and Display
  (font size) pages, plus an About page with this changelog.
- Clear asks first, and Rename opens with the keyboard up.
- The ¶ button is gone; paragraphs break on pauses.
- Install and update with [Obtainium](ANDROID.md#install).

## v0.1.1 (2026-10-07)

### Scribe for Android

- First release: live transcription on the phone with the same models
  as scribe, from the mic, other apps' audio, or both; shared audio and
  video files; threads; Bluetooth headset mics and mic boost; recording
  in the background; and a voice typing keyboard. The APK is attached to
  each release.

### scribe

- Spoken digit strings are written as numerals.
- `scribe --chat` talks with an LLM through the new agent, with
  conversations kept as threads.
- An Android arm64 build of the command-line scribe (`just android`).

## v0.1.0 (2026-10-05)

- First release: scribe for macOS and Linux, and rpg_vox for Linux.
