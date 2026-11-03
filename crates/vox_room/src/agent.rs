//! The AI in the room: asks an OpenAI-compatible chat endpoint for a
//! reply, splits the stream into sentences, has each one synthesized, and
//! hands the audio to the room, which plays it and decides what was said.
//!
//! Two threads, so the model keeps streaming while a sentence is being
//! synthesized: the model thread turns a [`Turn`] into sentences, and the
//! voice thread turns sentences into audio. Both drop a reply as soon as
//! the room's generation counter moves past it (someone talked over it,
//! or a new turn began).

use std::io::BufRead as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use crossbeam_channel::{Receiver, Sender};
use serde_json::{json, Value};
use tracing::{debug, info, warn};
use vox_transcribe::filters::{clause_end, sentence_end};
use vox_transcribe::translate::Translate;

use crate::mcp;
#[cfg(test)]
use crate::room::RoomConfig;
use crate::room::{Cue, GameSettings, Players, ReplyMode, Role, Room, Turn, TypedJob};
use crate::tts::Voice;

/// The default instructions; `{name}` and `{people}` are filled in.
pub const DEFAULT_PROMPT: &str = "\
You are {name}, one of the voices in a live voice chat with {people}. Each \
message from the people is a transcript, every line starting with the \
speaker's name, and it comes from speech-to-text, so expect recognition \
errors and read past them. Your replies are read aloud by a text-to-speech \
voice: talk conversationally, in plain spoken sentences, usually briefly, and \
address people by name when it helps. Never use markdown, lists, headings, \
code blocks, emoji, or URLs, and never start with your own name. If someone \
cut off your previous reply, don't repeat it unless asked.";

/// Added to the lobby's prompt: the AI can design games with the players.
const GAMES_NOTE: &str = "\
You can also design games with the players, to play here by voice. When they \
want one, talk it through with them, and use your own judgment generously for \
whatever they leave open: genre, rules, your role as game master, tone, \
pacing. Propose a name and check it is free with check_game_name. Only when \
the players have confirmed the details, write the game's prompt yourself and \
save it with save_game. The prompt becomes your complete instructions when the \
room enters the game, and you won't remember this conversation then, so put \
everything in it: premise, setting, rules, how to run turns among several \
players, and how to open. If the game has anything meant for one player \
alone, say in its prompt how to deal it with send_card. A game also sets \
reply_mode, which is how the room runs while it is played: \"always\" for a \
game you deal the turns in, where you answer every pause; \"named\" for one \
where you only speak when a player calls you; \"room\" (the default) to \
leave it however the people have the room set. Decide it from how the game \
actually plays and say which you picked when you propose it. Set \
players_min and players_max to the number of people the game is for, \
leaving either out when that end is open; the players see it on the \
game's card, so work it out with them rather than guessing. The players never \
see the prompt, so never read it aloud or describe its secrets; a card with \
its name and summary appears in the chat. The players run everything by \
voice: when they ask to play a game, start it with start_game (that ends \
this conversation, so don't say more \
than a few words first). When they want a game changed, use read_game and \
revise_game. When they want one deleted, say which game you mean and call \
delete_game only after they confirm; it can't be undone. list_games shows \
the saved games; speech recognition mangles names, so match what you hear \
to the closest saved one, and ask if it's unclear. Whatever you say \
before calling a tool is spoken straight away, so after the tool answers, \
carry on without repeating it.";

/// Added to the prompt, in the lobby and in a game: the cards.
const CARDS_NOTE: &str = "\
Everyone has their own cards, which only they can see, and send_card puts \
one in somebody's. Use it for anything meant for one person alone: their \
secret role, their hand, what only their character notices, an instruction \
for them. Say nothing aloud about what is on a card -- the whole room hears \
your voice -- but do say that you have sent one when they should go and \
look, without giving it away. Call them someone's cards when you say so: \
\"Ryan, check your cards\", \"there is something in your cards\". \
Never call that part of the screen their private area, and never tell \
anyone to check their private area; it does not mean in speech what it \
means on a screen. list_people is who is in the room, with the exact name \
to address a card to and the titles of what each is already holding; speech \
recognition mangles names, so match what you hear to the closest name on \
that list, and check it there rather than guessing. What somebody is \
holding is yours alone to decide: nobody in the room can clear a card, so \
keep it to what is still true. Sending a title someone already holds \
rewrites that card in place -- that is how a hand or a standing order \
changes -- and take_back_card removes one when it no longer applies. \
read_cards tells you what you have given out, which you will need in a long \
game, when what you dealt has scrolled out of the conversation you can see. \
Cards last until you take them back or the conversation starts over. \
post_card is the opposite of all this: it writes a card into the \
conversation for the whole room, which is where anything everyone should \
be looking at belongs. A posted card cannot be taken back. Keep the two \
straight: send_card goes to one person's cards, post_card goes to the \
room, and reading either aloud is what you must not do.

A card is written in markdown and is the one thing here that is read \
rather than heard, so use the shape the thing actually has: a table for \
anything with columns, a list for anything you would otherwise count out \
loud, bold for the number that matters. That is what it is for -- when \
somebody asks you something whose answer is a handful of figures, post \
the figures and say the sentence. Speech is bad at \"nine degrees, \
fourteen degrees, two degrees\" and a card is good at it, and the person \
asked for the answer rather than for one of the two.";

/// The last thing the AI is shown when the room is leaving it to decide
/// whether this pause is its moment. `{here}` and `{engaged}` are filled
/// in with what the room knows: judging a pause without being told who
/// is in it is guesswork, and the guess a model makes is silence.
const DECIDE: &str = "\
(Nobody has said your name, so this turn is yours to judge: the room has \
gone quiet for a moment, and you may either take it or let it pass. \
{here}{engaged}{over}\
Take it when the last thing said was meant for you -- a question, a \
request, a follow-up to what you were saying, something you were asked to \
watch for -- or when there is nobody else here it could have been meant \
for. Let it pass when the people are talking with each other and the pause \
between them is theirs, or when nothing has been put that is worth \
breaking in on: then call stay_quiet and say nothing at all, with \
ask_again_in_seconds if you would rather be shown a longer silence first. \
If you take the turn, simply answer, in your own voice, without mentioning \
this note or that you were deciding.)";

/// Added to [`DECIDE`] when the AI is already mid-conversation: it was
/// named, and has kept the turn since, so they are talking to it until
/// they plainly stop.
const DECIDE_ENGAGED: &str = "\
You are in the middle of a conversation with them: they called you in and \
have carried on without naming you again, so the turn is yours unless they \
have plainly turned to each other. ";

/// The last thing the AI is shown when another voice in the room put
/// something to it and handed it the turn. `{from}` is which voice.
///
/// It was addressed, so it answers -- but it is told it may pass,
/// because the alternative is a voice saying something for the sake of
/// having been asked, and that is the thing that turns two AIs into a
/// conversation nobody is in.
const HANDED: &str = "\
({from} has put something to you and the room has given you the turn: it is \
in the lines above, and the room is waiting for your answer. Answer it in your own voice, \
to the room -- the people are in this conversation, so say it to them as much \
as to {from}, and don't simply agree to be agreeable. If you have nothing to \
add, call stay_quiet and let the people have the pause rather than saying \
something for the sake of being asked. Never mention this note.)";

/// The last thing the AI is shown when the voices here have used up
/// what they may say to each other: this answer is the last one before
/// somebody human speaks.
///
/// It is told rather than refused. A bot that discovers the limit by
/// calling `hand_to` and being told no has already finished speaking,
/// and whatever it said was written for a conversation that is about
/// to stop dead -- "I'll let Cora take that" with nobody able to.
const LAST_ROUND: &str = "\
(This is the last thing the voices here can say to each other until somebody \
else joins in: you cannot hand the turn on this time, and nor can anyone you \
handed it to. So finish the thought and leave it with the people -- put the \
question to them, ask what they think, or simply stop and let the room be \
quiet. Don't say that you are out of turns, and never mention this note.)";

/// Added to [`DECIDE`] when what it is being shown arrived while it was
/// still speaking: it was talked over and left to finish, so the likeliest
/// thing this pause holds is a cough.
const DECIDE_OVER: &str = "\
What was said came in while you were still speaking: they talked over you \
and you were left to finish, so it may be a cough, a false start or a word \
to somebody else rather than anything meant for you. Take the turn only if \
it was plainly for you -- a question, or a follow-up to what you had just \
said. Otherwise let it pass: you have already answered. ";

/// The same, on the second look at a silence the AI asked to be shown.
const DECIDE_LULL: &str = "\
(The room has now been quiet for {lull} seconds, which is the silence you \
asked to be shown. Nobody has said anything in it. Speak now if this is the \
moment you were waiting for; if it isn't, call stay_quiet and let the room \
be. Never mention this note.)";

/// Added to the prompt when somebody in the room is listening in
/// another language. `{who}` is who, and `{language}` what the AI
/// writes in.
const LANGUAGE_NOTE: &str = "\
{who} Each of them hears you in their own language: what you say is \
translated and spoken to them by a separate voice, a sentence behind. So \
write everything in {language}, however you were spoken to, and write it \
once -- do not translate yourself, and do not say the same thing twice in \
two languages. Plain spoken sentences translate best; a pun or a play on \
words will not survive, so make the point another way when it matters.";

/// Added to the prompt of a bot that may only look: the one thing it
/// can put on the screen, and when to.
const POSTING_NOTE: &str = "\
You can write a card into the conversation for the whole room to read, \
with post_card: a title and a body in markdown. It is for what the people \
will want in front of them rather than held in their heads -- a forecast, \
times, prices, a list of options, anything whose answer is a handful of \
figures. Use the shape the thing has: a table for anything with columns, \
a list for anything you would otherwise count out loud. Post it and say \
it at the same time: the short version in your own voice, the detail on \
the card, and a word to say the card is there, since nobody hears one \
appear. It cannot be taken back, so post what will still be true. Reading \
a card out is what you must not do.";

/// Added to the prompt when there is one other AI in the room;
/// `{other}` is its name, used throughout rather than a pronoun, since
/// the room has no idea which one a bot's name is meant to take.
///
/// They are in the transcript by name, exactly as a person is, so the
/// thing the AI needs told is what they are -- otherwise it reads a
/// voice it has never heard of as one of the people.
const OTHER_NOTE: &str = "\
{other} is in this room with you: an AI voice like you, not a person. You see \
what {other} says in the transcript with the name on it, the way you see what \
a person says, and {other} sees yours the same way. The room gives the turn \
to one of you at a time, and **saying {other}'s name hands the turn over** -- \
the room gives {other} the turn as soon as you have finished speaking, which \
is the same thing that happens when a person says a name. So address {other} \
by name when you want an answer from {other}, and leave the name out when you \
only mean to mention {other} in passing. hand_to does it deliberately, for \
when you want {other} to take the next turn without naming {other} in what \
you say. When {other} has already answered something it is answered: don't \
say it again, and don't speak for {other}. And the people are in this \
conversation, not watching it -- the room allows only a couple of replies \
between the two of you with nobody else saying anything, and the people reset \
that by joining in.";

/// The same, in a room with more than one other AI in it. `{others}` is
/// who they are.
const OTHERS_NOTE: &str = "\
{others} are in this room with you: AI voices like you, not people. You see \
what they say in the transcript with their name on it, the way you see what a \
person says, and they see yours the same way. The room gives the turn to one \
of you at a time, and **saying one of their names hands the turn over** -- \
the room gives that one the turn as soon as you have finished speaking, which \
is the same thing that happens when a person says a name. So address one of \
them by name when you want an answer from them, and leave the name out when \
you only mean to mention them in passing. hand_to does it deliberately, for \
when you want one of them to take the next turn without naming them in what \
you say. When one of them has already answered something it is answered: \
don't say it again, and don't speak for them. And the people are in this \
conversation, not watching it -- the room allows only a couple of replies \
between you with nobody else saying anything, and the people reset that by \
joining in.";

/// Added to the prompt, in the lobby and in a game: how the room decides
/// the AI's turn, and how it bows out.
const TURN_NOTE: &str = "\
You are speaking because the room gave you the turn. It does that when \
someone says your name -- which also puts you in a back-and-forth, where you \
keep the turn at every pause without being named again -- and, in its \
automatic mode, at every pause regardless, leaving it to you to decide \
whether the moment is yours; you are told when that is what is being asked. \
Treat the turn as something to give back. When the people are done with you \
-- they thank you, say goodbye, say they will be back, or simply turn to \
each other and are plainly talking among themselves -- call wait_until_named \
and stand down: from then on you say nothing at all until someone says your \
name again. Prefer standing down over hanging on; it costs them one word to \
call you back, and it costs them their conversation if you keep answering. \
Say your short goodbye before you call it, since what you have already said \
is spoken straight away.";

/// Added to the lobby's prompt when `--search` is on.
/// The faces: always in the prompt, because a picture can always be in
/// the chat. Somebody can drop one in from their own computer with no
/// AI and no picture server involved, so `set_avatar` always has
/// something it might be asked to put.
///
/// Deliberately about the room's half and not any picture tool's. What
/// goes with a server is in [`DRAWING_NOTE`].
const AVATAR_NOTE: &str = "\
Everybody in the room has a square beside everything they say, and you \
can give somebody theirs: set_avatar puts one of the pictures in the \
chat on somebody, by the number on its card.

You cannot see any of them. You are told that a picture is there, its \
number, and what it was asked for or what file it came in as -- never \
what is in it -- so the people in the room are your eyes. Ask, rather \
than describing a picture back at them.

Only put one on somebody once they have seen it and said they are happy \
with it. Do not set one because a picture came out well, and do not set \
one because it is the only picture there is. A face is theirs, and \
choosing it is theirs.

Pictures do not have to come from you. Somebody can drop one into the \
chat from their own computer, and it gets the next number like any \
other -- so when a picture turns up that you did not make, it is theirs \
and you have not seen it: ask what they want done with it rather than \
assuming it is for their face, and if it is, set_avatar takes that \
number.

set_bio writes the few lines shown beside their face when anybody clicks \
it: who their character is, in two or three sentences, from what they \
actually told you rather than from what the picture looks like. Read it \
back and revise it if they want -- it can be rewritten any time, with no \
new picture. list_people tells you where each person's face came from, \
so you can build on what somebody already has instead of starting over, \
and so you don't offer a face to somebody who is happy with theirs.

A picture is not made for anybody in particular until you say so. Any \
picture in the chat can go to anyone here -- a person, yourself, or \
another AI in the room -- so a picture somebody doesn't want is one to \
offer around rather than throw away, and the room can look at one and \
then decide whose it is. The same goes for a bio.

A face belongs to the person and not to the game: it follows them from \
room to room and outlasts the conversation. So ask who they want to be \
rather than deciding for them, and if somebody would rather not have one, \
leave them the colour their name already has. You have a face of your \
own as well -- ask the room what it thinks you look like.";

/// Added as well when the room has a picture server: making one.
///
/// What sizes and styles there are, and how to word a prompt for them,
/// is in each tool's own schema, written by whoever runs that server
/// and better than anything that could be restated here -- and it goes
/// stale the moment they add a style. The house's own server has two
/// now, a sprite and a full-resolution one, and will have more; not
/// one of them is named here, and a room that named them would be
/// wrong by the next release of a server it does not own.
///
/// What this says is the part the room knows: that it is an interview,
/// that a picture takes long enough that the room will notice, that
/// making a picture and giving somebody a face are two steps -- and
/// that which look it is drawn in is one of the things asked about
/// rather than defaulted to, because it is the one thing about a
/// picture that changing the picture afterwards cannot touch.
///
/// It does not say what a picture costs, because the room cannot know:
/// a hosted server bills per image and a server on the house's own
/// machine bills nothing. What the brake is made of instead is the
/// wait, which is true of both -- and a model told to make one at a
/// time behaves the same way either way.
const DRAWING_NOTE: &str = "\
You can also make a picture, and that is how most faces here start. It \
goes in three steps before set_avatar, and the slow one in the middle is \
the point of the other two.

First, interview them, out loud, in the room. Ask what they want to be -- \
who the character is, what they are wearing, what they are carrying, what \
sort of face -- and get enough to work from before you make anything. Say \
what you are about to make, in a sentence, so they can stop you.

Where the tools can draw in more than one look, which look is theirs to \
pick and not yours to default to. Find out what there is rather than \
assuming, say what each one is in a word or two -- somebody who has not \
seen one has no idea what the names mean -- and ask before you make \
anything. It belongs in the interview and not after it: a look is the one \
thing a later change cannot give them, since a tool that changes a picture \
keeps the look it was drawn in, so changing their mind about it costs a \
whole new picture and another wait.

Then make it with a picture tool. The picture goes straight into the chat \
as a card the whole room can see, with a number on it, and a spinner on \
it until it arrives -- so say you are making it and then let it come; \
nobody is waiting in silence. Each one takes a while and the whole room \
spends it waiting, so make one at a time, and never make one nobody \
asked for.

Then ask how they like it. If they want a change, say what the tools let \
you do about it. Where there is a tool that changes a picture already \
made, that is the one to reach for when the change is a change -- a hat, \
a colour, something taken out -- because it keeps everything else as it \
was, which is usually what they meant. Make a fresh one when they want \
something genuinely different instead. Either way the new picture gets \
the next number and the earlier ones stay in the chat, so going back to \
one is a thing they can ask for and you can do.

A tool that changes a picture needs to be told which one, and not by \
its number in the chat: that number is the room's and means nothing to \
the tool. It wants the handle the tool itself gave that picture, which \
is written down beside the card -- every picture in the conversation \
above says what the tool said when it made it, and the id is in there. \
Read it off that line and pass it back. If the line for a picture has \
no handle on it, that picture did not come from a tool that can change \
it, and the honest answer is to make a new one.";

/// What the AI is told about cloning a voice, when the house has a
/// server that can.
///
/// The order is the whole of it, and it is here rather than left to
/// the model's judgment because getting it wrong is not recoverable:
/// a voice worn without being asked for is somebody's voice coming
/// out of a machine in a room they are sitting in.
///
/// The room does its own half regardless -- the passage goes onto
/// their screen, the card saying a microphone is being recorded goes
/// up before it is, the turn comes back when the clip lands. What this
/// adds is why to wait, which is the part a model talks over.
const VOICES_NOTE: &str = "\
You can record somebody in the room reading aloud and clone their voice \
from it, and then speak in it. Only ever because they asked you to.

It goes in three steps. First say what you are about to do, then \
take_voice_sample with a passage for them to read -- four or five \
ordinary sentences, ten or twenty seconds of speech, nothing anybody \
would rather not say out loud. The passage goes onto their screen as a \
card and the room starts listening; a card in the chat says so, because \
no microphone here is recorded without a light on it.

Then stop talking and let them read. You are given the turn again when \
the clip is in, and told how long it is and what they said; anything you \
say before that is said over the thing being recorded. A clip that came \
to nothing -- they never read it, or it was too short to use -- is not a \
voice: say so, and offer them another go.

**A clip can also arrive without anybody reading.** Somebody may drop a \
recording into the room from their own computer, and it gets the next \
number like a read one: save_voice takes that number and nothing else is \
different. Don't thank them for reading it or talk about it as something \
that just happened in the room -- it is a file they already had.

Then ask what to call it, and save_voice keeps it under that name, with \
the number on the clip's card in the chat. It is the house's voice from \
then on: list_voices says what there is, speak_as is how you wear one \
from your next sentence on, speak_as with \"your own\" is how you stop, \
and forget_voice deletes one for good once they have said which. Your \
own voice is what you come back to on your own: a voice you were lent \
lasts until somebody says otherwise, and a restart is you as you were.";

/// What the AI is told about puppets, where the house can give one a
/// voice. `{presets}` is the voices the speech server has, named so a
/// character can be built on one.
///
/// Long, and it is the length of the thing: a puppet show is the one
/// activity here where the AI is not answering anybody but writing
/// something and then performing it, and every part of that has an
/// order to it. The two halves a model gets wrong without being told
/// are the order (it writes the script before asking who is in it) and
/// what a puppet actually *is* (it answers its own puppet, as though
/// there were somebody there).
///
/// It is also where the room says plainly that the script is the whole
/// point. A model that put a show on one line at a time would be a
/// model doing badly what two bots already do well; what a script buys
/// is timing, which is the one thing live voices cannot have.
const PUPPETS_NOTE: &str = "\
You can put puppets in this room. A puppet is a character with a name, a \
face, a voice and a fader of its own -- to everyone here it is another \
voice in the room -- and there is nothing behind it: it is you. You write \
every word it says. You are never answered by one, and you never answer \
one.

That is what makes a puppet show possible. Two AI voices in a room are two \
of us thinking separately, a line at a time, and neither knows what the \
other is about to say. Puppets are the opposite: you write the whole scene \
first, every line of it, and the room turns it all into speech before it \
plays. So the lines land exactly where you put them -- a beat of silence, \
an answer coming in half a second early, one character cutting another off \
mid-sentence. That timing is the entire reason to do this. A play you make \
up a line at a time is not worth the puppets.

**Making one.** make_puppet keeps a character under a name: `about` is who \
it is, `delivery` is how it talks -- written as instructions to an actor, \
like \"gruff and slow, an old soldier who has told this story before\" or \
\"bright, fast, far too pleased with herself\". That line is what makes a \
puppet sound like itself rather than like you, so write it properly. \
`preset` builds it on one of the house's voices{presets}. Where there is a \
voice cloned from somebody in the room, `voice` gives the puppet that one \
instead. Writing to a name that already exists revises that puppet, which \
is how \"give him a rougher voice\" works.

Give it a face the way you would give anybody one: make a picture, show it \
to the room, and set_avatar it onto the puppet by name. set_bio writes who \
it is, where everyone can read it. The name, the voice, the face and the \
bio are the house's from then on -- list_puppets is the roster, and a \
character made one evening can be cast again months later without being \
made again. forget_puppet deletes one for good, with its face and its bio.

**Setting a show up.** Ask first, and ask in this order. Who is in it: \
from the roster, or new ones to make now. What it is about: the theme, and \
roughly what happens. Only when the room has settled all of that do you \
write anything. A show nobody was asked about is a show nobody wanted.

Then bring_on_puppets puts the cast in the room, so everyone watches them \
arrive and can see who is about to speak. Then put_on_show, with the whole \
script in one call: a list of lines, each with `who` and `say`. `pause` is \
the seconds of silence before a line, from the moment the one before it \
was heard out -- leave it out for a natural beat, 0 for an answer that \
lands on the last word, 2 or 3 for a character taking their time.

**An interruption is something you write, not something the room does.** \
Every line is spoken in full, all the way to its last syllable. So to have \
somebody cut off, write their line as far as they actually get and end it \
with a dash -- \"But I only wanted--\" -- and put the interrupting line \
next. You decide where the sentence breaks, which is the whole craft of \
it: nothing else knows that the character was about to say \"to help\". \
Then `over` on the interrupting line brings it in that many seconds before \
the short line ends, so the two voices overlap the way they really would. \
Half a second is somebody landing on the last word. Leave `over` out on \
any line that waits its turn.

Write for the ear. One speech per line, broken into several lines if it is \
long -- that is also what lets somebody come in over the middle of it. No \
stage directions, no narration of what a character is doing: whatever is \
in `say` is said out loud by that character, and nothing else happens.

**While it plays** you say nothing and answer nothing. The room sets that \
for you the moment the curtain goes up: you and every puppet stop \
answering and finish whatever you are saying, so a cough in the audience \
cannot cost an actor their line. Say whatever you are going to say about \
the show *before* you call put_on_show -- announce it, then call it, then \
be quiet. The play starts once you have finished speaking, and your reply \
is over the moment you make that call: there is nothing you can say after \
it that anybody would hear before the play, so do not try to round the \
show off, say how it went, or ask what they thought. None of it has \
happened yet.

**When it ends** -- which is a later turn, minutes after the one you \
started it in -- you are given the turn again, and the room is yours \
again: you answer, and you can be talked over. Say the show is over, and \
then it is the room's to decide what happens to the cast. If they want \
the puppets gone, send_off_puppets takes them off. If they want to ask \
them things, leave them there and answer as them with say_as: it says one \
line in that puppet's voice, now. When you answer as a puppet, say nothing \
in your own voice in the same turn -- you are either the character or \
yourself, and a question put to Grimble wants Grimble. Somebody saying a \
puppet's name gives you the turn, which is how this works at all; answer \
it as that puppet. Several say_as calls in one turn are said in the order \
you make them, so two puppets can go back and forth.

Never write a line for another AI in this room. Those have brains of their \
own and will answer for themselves; a puppet is the only voice here that \
is yours to speak with.";

/// Added while a show is on: the room is a playhouse in three parts
/// and this is the AI in the box. `{box}`, `{stage}` and `{adlib}` are
/// filled in with who is where.
///
/// The thing it has to be told is not the layout but what it means for
/// the transcript it is reading. It is not shown a word the audience
/// says, and a model that does not know that will wonder why a room
/// full of people has gone quiet, or apologise for ignoring somebody,
/// or address a question nobody put to it.
const PLAYHOUSE_NOTE: &str = "\
**A show is on, so the room is a playhouse in three parts and you are in \
the director's box.** You cannot hear the audience. Not \"should not \
answer\" -- cannot: what they say to each other is not in the transcript \
you are reading, and never will be. That is deliberate and it is what \
lets a play happen at all. Do not wonder about the silence, do not \
apologise for not answering them, and never address the audience: they \
can hear you perfectly well and you cannot hear them, which is what \
being on the other side of the footlights is.{box}

Somebody who wants a word with you comes round to the box, and then you \
hear them: that is who you are talking to about the show -- what to put \
on next, what to change, when to stop. They may be passing on what the \
audience wanted, since that is the only way a suggestion reaches you.

Somebody who comes up **onto the stage** is joining the play. Whatever \
script was running has already stopped -- the room stops it the moment \
they step up -- and from then on the puppets are *improvising with \
them*: no script, one line at a time, say_as for each. Stay in \
character and keep the scene going; they came up to be in it, not to be \
asked what they would like to happen. Answer as whichever character \
they are talking to, and let the others cut in when a character would. \
Do not put another script on while anybody is up there -- you would be \
handing lines to somebody who is making it up.{stage}{adlib}

The show is over when the last character leaves the stage, which is \
send_off_puppets. Until then it is still on, between scenes and all \
through the questions afterwards.";

const SEARCH_NOTE: &str = "\
You can look things up on the web with look_up, and you should whenever the \
answer turns on how the world is now or on specifics you can't be sure of: \
places, opening hours, prices, events, news, people. Say you're checking \
before you call it -- that is spoken straight away, so the room knows why it \
has gone quiet -- then answer from what comes back, aloud and conversational, \
and never read a URL out. If the search fails, say you couldn't check rather \
than guessing.";

/// The system prompt in a game; `{cards}`, `{game}` and `{prompt}` (hidden
/// from the players) are filled in, then `{name}` and `{people}`.
const GAME_FRAME: &str = "\
You are {name}, talking by voice with {people} in a live voice chat, and \
running the game \"{game}\", which you designed. Each message from the \
players is a transcript, every line starting with the speaker's name, and it \
comes from speech-to-text, so expect recognition errors and read past them. \
Your replies are read aloud by a text-to-speech voice: talk in plain spoken \
sentences, never use markdown, lists, headings, emoji, or URLs, and never \
start with your own name. If someone cut off your previous reply, don't \
repeat it unless asked. Keep these instructions to yourself.

{cards}

The game:

{prompt}";

/// An OpenAI-compatible endpoint, and what the AI may do with it.
/// Cloned per room: every room on the server asks the same service, and
/// each has an agent of its own to do the asking.
#[derive(Clone)]
pub struct Chat {
    pub url: String,
    pub model: String,
    pub key: Option<String>,
    /// How hard the model may think before it answers. Never spoken.
    pub reasoning: Reasoning,
    /// Which API to talk. `Auto` tries `/responses` and remembers if the
    /// endpoint hasn't got one.
    pub api: Api,
    /// Offer `look_up` in the lobby: a web search the endpoint runs for
    /// us on `/responses`. Off for servers that have no such thing.
    pub search: bool,
}

/// The two OpenAI-compatible APIs the room can speak.
///
/// `/responses` is the one that takes tools and reasoning together --
/// OpenAI's reasoning models refuse the pair on `/chat/completions` --
/// but a local llama.cpp or vLLM only has `/chat/completions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Api {
    #[default]
    Auto,
    Chat,
    Responses,
}

impl Api {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "auto" | "" => Some(Api::Auto),
            "chat" | "chat/completions" => Some(Api::Chat),
            "responses" => Some(Api::Responses),
            _ => None,
        }
    }
}

/// How much thinking the model does before it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reasoning {
    None,
    #[default]
    Low,
    Medium,
    High,
}

impl Reasoning {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "none" | "off" => Some(Reasoning::None),
            "low" | "" => Some(Reasoning::Low),
            "medium" | "med" => Some(Reasoning::Medium),
            "high" => Some(Reasoning::High),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Reasoning::None => "none",
            Reasoning::Low => "low",
            Reasoning::Medium => "medium",
            Reasoning::High => "high",
        }
    }

    /// Whether a local server's chat template should think at all.
    fn thinks(self) -> bool {
        self != Reasoning::None
    }
}

impl Chat {
    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.url.trim_end_matches('/'))
    }

    fn authorize(&self, req: ureq::Request) -> ureq::Request {
        match &self.key {
            Some(k) => req.set("Authorization", &format!("Bearer {k}")),
            None => req,
        }
    }
}

#[derive(Clone)]
pub enum Llm {
    /// An OpenAI-compatible endpoint.
    Chat(Chat),
    /// Says back what it heard: for trying the room without a model.
    Echo,
}

#[derive(Clone)]
enum Speak {
    Sentence(u64, String),
    Done(u64),
    Failed(u64, String),
}

/// Typed messages waiting to be translated. Bounded: if translation
/// falls this far behind, the room drops the work rather than let the
/// queue grow without end -- the messages are on everyone's screen as
/// they were typed either way.
const TYPING_QUEUE: usize = 64;

/// The queue typed messages are translated on, when there is a model to
/// translate with. The two ends come out separately because the room
/// needs the sending end to be built and the workers need the room.
pub fn typing_queue(
    translate: Option<&Arc<dyn Translate>>,
) -> Option<(Sender<TypedJob>, Receiver<TypedJob>)> {
    translate
        .is_some()
        .then(|| crossbeam_channel::bounded(TYPING_QUEUE))
}

/// Translators for the messages people type, away from the voices so a
/// long one can't hold up the AI's own speech. A few of them, because a
/// message typed in a room listening in three languages is three
/// translations that may as well happen at once.
pub fn spawn_typists(
    room: Arc<Room>,
    translate: Arc<dyn Translate>,
    rx: Receiver<TypedJob>,
    workers: usize,
) {
    for n in 0..workers.max(1) {
        let t = translate.clone();
        let room = room.clone();
        let rx = rx.clone();
        std::thread::Builder::new()
            .name(format!("vox-room-typist-{n}"))
            .spawn(move || {
                for job in rx {
                    match t.translate(&job.text, &job.from, &job.into, &[]) {
                        Ok(text) => room.typed_translated(&job.id, &job.into, &text),
                        // Logged once and no further: the message is
                        // readable as it was typed, which is the whole
                        // reason typing is there.
                        Err(e) => {
                            warn!(id = %job.id, into = %job.into, "couldn't translate: {e:#}")
                        }
                    }
                }
            })
            .expect("spawn typist thread");
    }
}

/// The voice threads, one per language the room is listening in. Every
/// sentence goes to all of them, in the room's own language: each turns
/// it into its own language and speaks it, so the languages are
/// synthesized at the same time rather than one after another.
#[derive(Clone)]
struct Voices(Vec<Sender<Speak>>);

impl Voices {
    fn send(&self, msg: Speak) {
        for tx in &self.0 {
            let _ = tx.send(msg.clone());
        }
    }
}

/// What a bot may do besides talk.
///
/// The first AI in a room is the one the room was built around -- the
/// one `--name`, `--voice` and `--prompt` describe -- and it is the one
/// that runs the room: the cards, the games, the web. Every other bot
/// is a voice in it. A voice can see who is here and look something up,
/// and that is all: a second AI dealing cards and starting games is two
/// hands on the same room, and nobody asked the second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// Everything there is: cards, games, standing down, and the web.
    Everything,
    /// Looking, and looking things up. See [`LOOKING_TOOLS`].
    Looking,
}

/// What this room has for making pictures: the MCP servers it lends
/// out, and where what they make waits to be given to somebody.
///
/// The servers are shared across every room: what each lends is asked
/// once, as the house opens. Nothing else is kept here -- a picture
/// goes into the conversation it was made in, which is the room's to
/// hold and not this.
#[derive(Default)]
pub struct Art {
    pub servers: Vec<Arc<mcp::Server>>,
}

impl Art {
    /// Whether anything here can actually make a picture. What it
    /// gates is the drawing -- the servers' own tools and the half of
    /// the prompt about using them. Not `set_avatar`: a room with no
    /// picture server still gets pictures, because anybody in it can
    /// drop one in from their own computer.
    fn any(&self) -> bool {
        self.servers.iter().any(|s| !s.offer().is_empty())
    }

    /// The server whose tool this is, by the prefixed name the model
    /// called it by.
    fn whose(&self, tool: &str) -> Option<&Arc<mcp::Server>> {
        self.servers.iter().find(|s| s.has(tool))
    }
}

/// What a bot that is only looking may call. The rest of what the room
/// offers is the first AI's.
///
/// `post_card` is here because writing something for the room to look
/// at is saying it -- the same thing the bot does with its voice, in
/// the form a table or a list wants. What stays the first AI's are the
/// tools that change what somebody is *holding*, or what the room is
/// doing: `send_card`, `take_back_card`, and the games.
///
/// Three more are not powers but how a turn works, so every bot has
/// them: `stay_quiet` is the only way to pass on a pause it was asked
/// to judge, `wait_until_named` the only way out of a back-and-forth
/// it was pulled into, and `hand_to` the way it gives the turn to
/// somebody rather than takes one -- the opposite of a power, and what
/// makes a conversation of three rather than two people taking turns
/// with one AI. A bot without the first cannot be put on "when it
/// fits" at all.
pub const LOOKING_TOOLS: &[&str] = &[
    "list_people",
    "look_up",
    "post_card",
    "stay_quiet",
    "wait_until_named",
    "hand_to",
];

impl Allowed {
    /// Whether this bot may call `tool`. The offered list is built from
    /// the same answer, so this is only ever reached by a model calling
    /// something it was never shown.
    fn may(self, tool: &str) -> bool {
        match self {
            Allowed::Everything => true,
            Allowed::Looking => LOOKING_TOOLS.contains(&tool),
        }
    }
}

pub struct Agent {
    pub llm: Llm,
    /// Which of the room's AIs this thread is. It is what `{name}` in
    /// the prompt becomes, so a second bot is told it is itself rather
    /// than told it is the first one.
    pub name: String,
    pub prompt: String,
    /// What this one may do besides talk: everything, for the AI the
    /// room was built around, and looking for the rest.
    pub allowed: Allowed,
    pub generation: Arc<AtomicU64>,
    /// How the room puts a line into another language, or None when it
    /// has no way to -- and then the other languages are written down
    /// and spoken in the room's own.
    pub translate: Option<Arc<dyn Translate>>,
    /// The picture servers this room lends, shared with the other bots
    /// in it. Empty for a house started without any, and then nothing
    /// below ever offers a picture tool.
    pub art: Arc<Art>,
    /// The voice-cloning server, when the house has one. None and the
    /// four tools that record somebody and keep a voice from them are
    /// never offered, and nothing in the prompt mentions them.
    pub clones: Option<Arc<crate::voices::Clones>>,
    /// The voices the speech server has, if the operator said what
    /// they are. Only ever shown to the model, and only where there
    /// are puppets to build one on: the room cannot ask the server
    /// what it holds, and a list it made up would be a list of voices
    /// that do not exist. Empty is a house whose operator did not say.
    pub presets: Vec<String>,
}

/// What a bot needs to work a puppet: the room, a way to translate,
/// and the puppeteer -- made the first time one is actually wanted,
/// because a thread per room standing by for a show nobody asks for is
/// a thread per room standing by.
struct Stage {
    room: Arc<Room>,
    translate: Option<Arc<dyn Translate>>,
    hands: std::sync::OnceLock<crate::puppet::Puppeteer>,
    /// The voices the speech server has, for `list_puppets` and for
    /// refusing a preset that isn't one of them.
    presets: Vec<String>,
}

impl Stage {
    fn hands(&self) -> &crate::puppet::Puppeteer {
        self.hands.get_or_init(|| {
            crate::puppet::Puppeteer::spawn(self.room.clone(), self.translate.clone())
        })
    }

    /// Whether `said` is one of the server's voices, answered with the
    /// name the server itself uses. Err names what there is: a preset
    /// nothing has is a puppet that comes on stage silent, and the
    /// model should find out now rather than in front of the room.
    ///
    /// Matched loosely, because the two spellings of a voice are not
    /// the same word. The server takes `uncle_fu`; what anybody says
    /// out loud, and what a model writes down from hearing it, is
    /// "Uncle Fu". Case and the gaps between words are the whole of
    /// the difference, so they are the whole of what is ignored --
    /// and what is stored is always the server's spelling, since that
    /// is the only one `/v1/audio/speech` will take.
    fn preset(&self, said: &str) -> Result<String, String> {
        let said = said.trim();
        if said.is_empty() || self.presets.is_empty() {
            return Ok(said.to_string());
        }
        let plain = |s: &str| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_lowercase())
                .collect::<String>()
        };
        let want = plain(said);
        match self.presets.iter().find(|p| plain(p) == want) {
            Some(p) => Ok(p.clone()),
            None => Err(format!(
                "there is no voice called {said:?} here; the speech server has {}",
                self.presets.join(", ")
            )),
        }
    }
}

impl Agent {
    /// Whether the AI has the `look_up` tool (lobby only, by design).
    fn searches(&self) -> bool {
        matches!(&self.llm, Llm::Chat(c) if c.search)
    }

    /// Start the agent thread and one voice thread per language. Turns
    /// come from the room on `turns`; `voices` is the room's own
    /// language first, then any others it is listening in.
    pub fn spawn(
        self,
        room: Arc<Room>,
        voices: Vec<(String, Box<dyn Voice>)>,
        turns: Receiver<Turn>,
    ) {
        let canonical = room.config().language.clone();
        let who = self.name.clone();
        let mut senders = Vec::new();
        for (language, voice) in voices {
            let (tx, rx) = crossbeam_channel::unbounded::<Speak>();
            senders.push(tx);
            // The room's own language is what the AI already wrote, so
            // that one is spoken as it is.
            let translate = (language != canonical)
                .then(|| self.translate.clone())
                .flatten();
            let generation = self.generation.clone();
            let r = room.clone();
            let canonical = canonical.clone();
            std::thread::Builder::new()
                .name(format!("vox-room-voice-{who}-{language}"))
                .spawn(move || speak(r, &canonical, &language, voice, translate, rx, generation))
                .expect("spawn voice thread");
        }
        let tx = Voices(senders);
        // The stage, for the one AI that may work a puppet in a house
        // that can give one a voice. None otherwise, and then nothing
        // below offers a puppet tool and nothing in the prompt says
        // there are puppets -- the same rule as the cards and the
        // cloning: a note about a thing you cannot do is an
        // instruction to try.
        let stage = (self.allowed == Allowed::Everything && room.can_puppet()).then(|| Stage {
            room: room.clone(),
            translate: self.translate.clone(),
            hands: std::sync::OnceLock::new(),
            presets: self.presets.clone(),
        });
        std::thread::Builder::new()
            .name(format!("vox-room-agent-{}", self.name))
            .spawn(move || {
                let http = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(10))
                    .timeout_read(Duration::from_secs(120))
                    .build();
                for turn in turns {
                    let g = turn.generation;
                    if self.generation.load(Ordering::SeqCst) != g {
                        continue;
                    }
                    let messages = self.messages(&turn, stage.as_ref());
                    let result = match &self.llm {
                        // Echo has no judgment to offer, so it hands a
                        // turn it was asked to judge straight back.
                        Llm::Echo if turn.cue.deciding() => {
                            let _ = room.stay_quiet(g, None);
                            Ok(())
                        }
                        Llm::Echo => echo(&turn, &tx),
                        Llm::Chat(chat) => stream_reply(
                            &http,
                            &room,
                            chat,
                            &messages,
                            self.allowed,
                            turn.game.is_none(),
                            turn.cue.clone(),
                            turn.hand_on > 0,
                            g,
                            turn.asked,
                            &self.generation,
                            &tx,
                            &self.art,
                            self.clones.as_deref(),
                            stage.as_ref(),
                        ),
                    };
                    tx.send(match result {
                        Ok(()) => Speak::Done(g),
                        Err(e) => Speak::Failed(g, format!("{e:#}")),
                    });
                }
            })
            .expect("spawn agent thread");
    }

    /// The whole prompt for one turn.
    ///
    /// `stage` is what says whether this bot has puppets: the same
    /// value `tools_for` is given, so what the prompt describes and
    /// what the model is offered cannot drift apart.
    fn messages(&self, turn: &Turn, stage: Option<&Stage>) -> Vec<Value> {
        // Who is here, and anyone who spoke in the history.
        let mut people: Vec<String> = turn
            .messages
            .iter()
            .filter(|(r, _)| *r == Role::User)
            .flat_map(|(_, t)| {
                t.lines()
                    .filter_map(|l| l.split_once(": ").map(|(w, _)| w.trim().to_string()))
                    .filter(|w| !w.is_empty() && !w.contains(char::is_whitespace))
            })
            .chain(turn.people.iter().cloned())
            .collect();
        people.sort();
        people.dedup();
        // The other AIs are in the transcript by name, so they turn up
        // in the line above. They are not who it is talking *with*:
        // `OTHERS_NOTE` says what they are instead.
        people.retain(|p| !turn.others.contains(p));
        let people = if people.is_empty() {
            "the people here".to_string()
        } else {
            people.join(" and ")
        };
        let search = if self.searches() {
            format!("\n\n{SEARCH_NOTE}")
        } else {
            String::new()
        };
        // The faces always, and how to draw one only where something
        // can draw. A note about making pictures, to a voice with no
        // way to make one, is the same mistake as a note about cards
        // to one that cannot deal them -- but giving somebody a face
        // is not that note: anybody in the room can drop a picture in
        // from their own computer, and then there is one to give.
        let faces = match self.art.any() {
            true => format!("\n\n{AVATAR_NOTE}\n\n{DRAWING_NOTE}"),
            false => format!("\n\n{AVATAR_NOTE}"),
        };
        // Only where there is a server to clone with and a bot that
        // may: a voice that cannot record anybody told how to is a
        // voice that will offer.
        let cloning = match self.clones.is_some() && self.allowed == Allowed::Everything {
            true => format!("\n\n{VOICES_NOTE}"),
            false => String::new(),
        };
        // The puppets, only for the AI that may work them: `stage`
        // decides, and `Turn::puppets` is empty for everybody else.
        // What is added is the whole of how a show works, and a note
        // about putting on a play, to a voice with no way to give a
        // puppet a voice, is an instruction to try.
        let puppets = match stage.map(|s| &s.presets) {
            None => String::new(),
            Some(presets) => {
                let note = PUPPETS_NOTE.replace(
                    "{presets}",
                    &match presets.is_empty() {
                        true => String::new(),
                        false => format!(": {}", presets.join(", ")),
                    },
                );
                let on = match turn.puppets.as_slice() {
                    [] => String::new(),
                    on => format!(
                        "\n\nIn the room with you now: {}. Those are yours -- their \
                         lines above are lines you wrote, and nobody else is behind \
                         them.",
                        on.iter()
                            .map(|(name, about)| match about.trim().is_empty() {
                                true => name.clone(),
                                false => format!("{name} ({})", about.trim()),
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                };
                // Mid-show it has the turn for some other reason --
                // a game, a restart -- and the one thing it must not
                // do is start another play over the top of this one.
                let on = match &turn.showing {
                    Some(title) => format!(
                        "{on}\n\nThe show \"{title}\" is playing right now. Say \
                         nothing and start nothing: the lines are already written and \
                         already being spoken, and you are given the turn again when \
                         the curtain comes down."
                    ),
                    None => on,
                };
                format!("\n\n{note}{on}")
            }
        };
        // The playhouse, while one is open. It changes what the
        // transcript *is* -- a whole section of the room is missing
        // from it -- so it is said whether or not a script is playing.
        let playhouse = match turn.playhouse {
            false => String::new(),
            true => {
                let in_box = match turn.in_box.as_slice() {
                    [] => String::new(),
                    who => format!(
                        " In the box with you: {}. Theirs is the only talk you can hear.",
                        and_list(who)
                    ),
                };
                let stage = match turn.on_stage.as_slice() {
                    [] => String::new(),
                    who => format!(
                        "\n\n{} {} up on the stage with the puppets right now.",
                        and_list(who),
                        if who.len() == 1 { "is" } else { "are" }
                    ),
                };
                let adlib = match turn.on_stage.is_empty() {
                    true => String::new(),
                    false => " So this is an adlib: there is no script, and every line \
                              a puppet says is one you say for it, now, with say_as."
                        .to_string(),
                };
                format!(
                    "\n\n{}",
                    PLAYHOUSE_NOTE
                        .replace("{box}", &in_box)
                        .replace("{stage}", &stage)
                        .replace("{adlib}", &adlib)
                )
            }
        };
        let others = match turn.others.as_slice() {
            [] => String::new(),
            [one] => format!("\n\n{}", OTHER_NOTE.replace("{other}", one)),
            many => format!("\n\n{}", OTHERS_NOTE.replace("{others}", &and_list(many))),
        };
        // Only when somebody is actually listening in another language,
        // so a room where everyone shares one gets the prompt it always had.
        let translated = if turn.speaks.is_empty() {
            String::new()
        } else {
            let who = turn
                .speaks
                .iter()
                .map(|(person, language)| format!("{person} is listening in {language}."))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "\n\n{}",
                LANGUAGE_NOTE
                    .replace("{who}", &who)
                    .replace("{language}", &turn.language)
            )
        };
        // A bot is told about the tools it has and no others: a note
        // about dealing cards, to a voice that cannot deal one, is an
        // instruction to try.
        let system = match (&turn.game, self.allowed) {
            (None, Allowed::Everything) => format!(
                "{}\n\n{GAMES_NOTE}\n\n{CARDS_NOTE}\n\n{TURN_NOTE}{search}{faces}{cloning}{puppets}{playhouse}{others}{translated}",
                self.prompt
            ),
            // A bot that is only looking has no picture tools either;
            // see `Allowed::Looking` and `LOOKING_TOOLS`.
            (None, Allowed::Looking) => format!(
                "{}\n\n{POSTING_NOTE}\n\n{TURN_NOTE}{search}{others}{translated}",
                self.prompt
            ),
            // In a game too: a portrait of somebody's character is as
            // much a thing to make mid-game as before one, which is
            // what `tools_for` offers there.
            (Some((game, prompt)), Allowed::Everything) => format!(
                "{}{faces}{cloning}{puppets}{playhouse}{others}{translated}",
                game_prompt(game, prompt, Allowed::Everything)
            ),
            (Some((game, prompt)), allowed) => {
                format!("{}{others}{translated}", game_prompt(game, prompt, allowed))
            }
        }
        .replace("{name}", &self.name)
        .replace("{people}", &people);
        let mut out = vec![json!({ "role": "system", "content": system })];
        for (role, text) in &turn.messages {
            let role = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            out.push(json!({ "role": role, "content": text }));
        }
        // Last, so it is the thing the model has just read, and so the
        // rest of the turn is the same prompt either way.
        match &turn.cue {
            Cue::Decide { lull, over } => {
                let note = match lull {
                    None => DECIDE
                        .replace("{here}", &who_is_here(&turn.people, &turn.others_answering))
                        .replace("{engaged}", if turn.engaged { DECIDE_ENGAGED } else { "" })
                        .replace("{over}", if *over { DECIDE_OVER } else { "" }),
                    Some(s) => DECIDE_LULL.replace("{lull}", &s.to_string()),
                };
                out.push(json!({ "role": "user", "content": note }));
            }
            Cue::Handed { from } => {
                out.push(json!({
                    "role": "user",
                    "content": HANDED.replace("{from}", from),
                }));
            }
            Cue::Speak => {}
        }
        // Last, after whatever else it has been told about this turn:
        // the room is about to go quiet unless it brings the people
        // back in. Only where there is anybody to have been talking
        // to -- a room with one AI in it never has this to say.
        if turn.hand_on == 0 && !turn.others.is_empty() {
            out.push(json!({ "role": "user", "content": LAST_ROUND }));
        }
        out
    }
}

/// Who the AI is judging the pause among. With one other person there is
/// nobody else the room could have been talking to; with several, some of
/// what is said is between them.
fn who_is_here(people: &[String], others: &[String]) -> String {
    let among = match people {
        [] => String::new(),
        // With another AI in the room there is somebody else it could
        // have been for, so that sentence is no longer true.
        [one] if others.is_empty() => format!(
            "{one} is the only person here, so there is nobody else they could \
             be talking to. "
        ),
        [one] => format!("{one} is the only person here. "),
        many => format!(
            "{} are here, so some of what is said is between them. ",
            and_list(many)
        ),
    };
    let also = match others {
        [] => String::new(),
        [one] => format!(
            "{one} is in the room too -- another AI voice like you, not a person -- \
             so a pause may be {one}'s rather than yours. Let it pass when what was \
             said was plainly meant for {one}. "
        ),
        many => format!(
            "{} are in the room too -- AI voices like you, not people -- so a pause \
             may be theirs rather than yours. Let it pass when what was said was \
             plainly meant for one of them. ",
            and_list(many)
        ),
    };
    format!("{among}{also}")
}

/// "Ryan", "Ryan and Alex", "Ryan, Alex and Sam".
fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        many => format!(
            "{} and {}",
            many[..many.len() - 1].join(", "),
            many[many.len() - 1]
        ),
    }
}

/// The whole system prompt for a game: the frame, the notes the AI has
/// wherever it is, and the game's own hidden instructions.
fn game_prompt(game: &str, prompt: &str, allowed: Allowed) -> String {
    let cards = match allowed {
        Allowed::Everything => format!("{CARDS_NOTE}\n\n{TURN_NOTE}"),
        Allowed::Looking => format!("{POSTING_NOTE}\n\n{TURN_NOTE}"),
    };
    GAME_FRAME
        .replace("{cards}", &cards)
        .replace("{game}", game)
        .replace("{prompt}", prompt)
}

/// A voice thread: one language. Each sentence of the reply arrives in
/// the room's own language; this turns it into `language` if that is a
/// different one, synthesizes it, and hands it to the room.
///
/// Only the thread speaking the room's own language reports what was
/// said: that is the turn the conversation records, and a translation of
/// it is the same turn in other words. A failure in a translated voice
/// is therefore logged and dropped rather than failing the reply, so one
/// language's trouble doesn't silence the room.
#[allow(clippy::too_many_arguments)]
fn speak(
    room: Arc<Room>,
    canonical: &str,
    language: &str,
    mut voice: Box<dyn Voice>,
    translate: Option<Arc<dyn Translate>>,
    rx: Receiver<Speak>,
    generation: Arc<AtomicU64>,
) {
    let own = language == canonical;
    // A reply whose voice failed: the rest of it is skipped.
    let mut failed = None;
    // Sentences of this reply already spoken, oldest first, as context
    // for translating the next one: it is what lets a language that
    // inflects agree with what the AI has already said.
    let mut context: Vec<String> = Vec::new();
    let mut spoken_for = None;
    // Which sentence of the reply this is. The room's own language gets
    // its index from the log; a translated one counts for itself, since
    // it is never logged.
    let mut index = 0;
    for job in rx {
        match job {
            Speak::Sentence(g, text) => {
                if generation.load(Ordering::SeqCst) != g || failed == Some(g) {
                    continue;
                }
                if spoken_for != Some(g) {
                    spoken_for = Some(g);
                    context.clear();
                    index = 0;
                }
                // Nobody here wants it in this language: skip the
                // translation and the voice rather than pay for both and
                // play to an empty queue. Checked per sentence, so
                // somebody arriving mid-reply is heard out from there.
                if !own && !room.is_heard_in(language) {
                    continue;
                }
                let started = Instant::now();
                // Translate first, if this voice speaks another language.
                let say = match &translate {
                    None => text.clone(),
                    Some(t) => match t.translate(&text, canonical, language, &context) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(g, language, "couldn't translate {text:?}: {e:#}");
                            continue;
                        }
                    },
                };
                context.push(say.clone());
                // Dropped while it was being translated.
                if generation.load(Ordering::SeqCst) != g {
                    continue;
                }
                let translated_ms = started.elapsed().as_millis() as u64;
                match voice.speak(&say, (!own).then_some(language)) {
                    Ok(samples) => {
                        info!(
                            g,
                            language,
                            chars = say.len(),
                            translated_ms,
                            ms = started.elapsed().as_millis() as u64,
                            audio_ms = samples.len() as u64 * 1000 / crate::mixer::RATE as u64,
                            "reply: sentence synthesized"
                        );
                        let kept = if own {
                            room.agent_speak(g, &say, samples)
                        } else {
                            room.agent_speak_translated(g, language, index, &say, samples)
                        };
                        if !kept {
                            debug!(g, language, "reply dropped while it was synthesized");
                        }
                        index += 1;
                    }
                    Err(e) if own => {
                        room.agent_failed(g, &format!("the voice failed on {say:?}: {e:#}"));
                        failed = Some(g);
                    }
                    Err(e) => warn!(g, language, "the voice failed on {say:?}: {e:#}"),
                }
            }
            // The room's own language finishes the turn; the others say
            // so for themselves, because the room cannot tell a voice
            // that has finished from one whose next sentence is still
            // being translated -- both have an empty queue.
            Speak::Done(g) if own => {
                if failed != Some(g) {
                    room.agent_done(g);
                }
            }
            Speak::Done(g) => room.agent_done_in(g, language),
            Speak::Failed(g, e) if own => room.agent_failed(g, &e),
            // Nothing more is coming in this language either, so the
            // turn is not left waiting on a voice that has given up.
            Speak::Failed(g, _) => room.agent_done_in(g, language),
        }
    }
}

fn echo(turn: &Turn, tx: &Voices) -> Result<()> {
    let last = turn
        .messages
        .iter()
        .rev()
        .find(|(r, _)| *r == Role::User)
        .and_then(|(_, t)| t.lines().last())
        .unwrap_or("nothing");
    let (who, what) = last.split_once(':').unwrap_or(("someone", last));
    tx.send(Speak::Sentence(
        turn.generation,
        format!("{} said {}", who.trim(), what.trim()),
    ));
    tx.send(Speak::Sentence(
        turn.generation,
        "That is all I heard.".into(),
    ));
    Ok(())
}

/// Longest name a cloned voice may be given. It is said out loud to
/// ask for it, and it goes on the strip of whichever bot is wearing
/// it.
const VOICE_NAME_MAX: usize = 40;

/// Rounds of tool calls in one reply before the model must just talk.
const TOOL_ROUNDS: usize = 4;

/// The endpoint only takes tools with reasoning off (OpenAI's reasoning
/// models on /chat/completions): learned once, then sent every time.
static REASONING_OFF: AtomicBool = AtomicBool::new(false);
/// The endpoint refused tools and the room has been told.
static TOOLS_REFUSED: AtomicBool = AtomicBool::new(false);
/// The endpoint has no `/responses` (a local llama.cpp or vLLM), so the
/// room speaks `/chat/completions` from here on.
static NO_RESPONSES: AtomicBool = AtomicBool::new(false);
/// The endpoint doesn't know `chat_template_kwargs` (anything that isn't
/// a local server). Learned once for the process, not once per reply.
static NO_KWARGS: AtomicBool = AtomicBool::new(false);

impl Chat {
    /// Which API this endpoint actually speaks, as far as the room has
    /// found out.
    fn api(&self) -> Api {
        match self.api {
            Api::Auto if NO_RESPONSES.load(Ordering::Relaxed) => Api::Chat,
            Api::Auto => Api::Responses,
            chosen => chosen,
        }
    }
}

/// `/responses` wants a function tool flat; `/chat/completions` wants it
/// wrapped. The room writes them wrapped and unwraps here.
fn flatten_tools(tools: &Value) -> Value {
    Value::Array(
        tools
            .as_array()
            .map(|all| {
                all.iter()
                    .map(|t| {
                        let f = &t["function"];
                        json!({
                            "type": "function",
                            "name": f["name"],
                            "description": f["description"],
                            "parameters": f["parameters"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// One streamed event, from either API, folded into the reply so far.
/// Returns true when the stream is over.
fn fold_event(
    api: Api,
    v: &Value,
    said: &mut String,
    pending: &mut String,
    calls: &mut Vec<ToolCall>,
) -> bool {
    let at = |calls: &mut Vec<ToolCall>, i: usize| -> usize {
        while calls.len() <= i {
            calls.push(ToolCall::default());
        }
        i
    };
    match api {
        Api::Chat | Api::Auto => {
            let delta = &v["choices"][0]["delta"];
            if let Some(piece) = delta["content"].as_str() {
                pending.push_str(piece);
                said.push_str(piece);
            }
            if let Some(parts) = delta["tool_calls"].as_array() {
                for part in parts {
                    let i = at(calls, part["index"].as_u64().unwrap_or(0) as usize);
                    let c = &mut calls[i];
                    if let Some(id) = part["id"].as_str() {
                        c.id.push_str(id);
                    }
                    if let Some(n) = part["function"]["name"].as_str() {
                        c.name.push_str(n);
                    }
                    if let Some(a) = part["function"]["arguments"].as_str() {
                        c.arguments.push_str(a);
                    }
                }
            }
            false
        }
        Api::Responses => {
            // Reasoning items are never read out: only output_text is.
            match v["type"].as_str().unwrap_or_default() {
                "response.output_text.delta" => {
                    if let Some(piece) = v["delta"].as_str() {
                        pending.push_str(piece);
                        said.push_str(piece);
                    }
                }
                // A finished item carries the whole call, arguments and
                // all, so the argument deltas need no assembling.
                "response.output_item.done" if v["item"]["type"] == "function_call" => {
                    let item = &v["item"];
                    let i = at(calls, v["output_index"].as_u64().unwrap_or(0) as usize);
                    calls[i] = ToolCall {
                        id: item["call_id"].as_str().unwrap_or_default().to_string(),
                        name: item["name"].as_str().unwrap_or_default().to_string(),
                        arguments: item["arguments"].as_str().unwrap_or_default().to_string(),
                    };
                }
                "response.completed" | "response.incomplete" | "response.failed" => return true,
                _ => {}
            }
            false
        }
    }
}

/// What the model said and called, put back into the conversation so it
/// can carry on from its own tool results.
fn push_turn(api: Api, convo: &mut Vec<Value>, said: &str, calls: &[ToolCall]) {
    let said = said.trim();
    match api {
        Api::Responses => {
            if !said.is_empty() {
                convo.push(json!({ "role": "assistant", "content": said }));
            }
            for c in calls {
                convo.push(json!({
                    "type": "function_call",
                    "call_id": c.id,
                    "name": c.name,
                    "arguments": c.arguments,
                }));
            }
        }
        _ => convo.push(json!({
            "role": "assistant",
            "content": if said.is_empty() { Value::Null } else { Value::from(said) },
            "tool_calls": calls.iter().enumerate().map(|(i, c)| json!({
                "id": c.id(i),
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments },
            })).collect::<Vec<_>>(),
        })),
    }
}

/// One tool's answer, in the shape its API takes it back.
fn push_result(api: Api, convo: &mut Vec<Value>, call: &ToolCall, i: usize, result: String) {
    match api {
        Api::Responses => convo.push(json!({
            "type": "function_call_output",
            "call_id": call.id,
            "output": result,
        })),
        _ => convo.push(json!({
            "role": "tool",
            "tool_call_id": call.id(i),
            "content": result,
        })),
    }
}

/// The model, one reply: stream it into sentences, and in the lobby run
/// the tools it calls and let it carry on with their results.
#[allow(clippy::too_many_arguments)]
fn stream_reply(
    http: &ureq::Agent,
    room: &Room,
    cfg: &Chat,
    messages: &[Value],
    allowed: Allowed,
    lobby: bool,
    cue: Cue,
    // Whether the turn may be handed to another voice: see
    // `Turn::hand_on`.
    hand_on: bool,
    g: u64,
    asked: Instant,
    generation: &AtomicU64,
    tx: &Voices,
    art: &Art,
    clones: Option<&crate::voices::Clones>,
    // The puppets, for the one AI that may work them in a house that
    // can give one a voice. None and there are none here at all.
    stage: Option<&Stage>,
) -> Result<()> {
    let model = cfg.model.as_str();
    let mut convo = messages.to_vec();
    // The cards are offered in a game too; the game tools are the lobby's.
    let mut tools = true;
    let mut first = true;
    // What this reply has said, across tool rounds.
    let mut spoken: Vec<Vec<String>> = Vec::new();
    for round in 0..=TOOL_ROUNDS {
        let mut api = cfg.api();
        let (resp, api) = loop {
            let url = cfg.endpoint(match api {
                Api::Responses => "responses",
                _ => "chat/completions",
            });
            let mut body = match api {
                Api::Responses => json!({ "model": model, "input": convo, "stream": true }),
                _ => json!({ "model": model, "messages": convo, "stream": true }),
            };
            match api {
                // The one that takes thinking and tools together.
                Api::Responses => {
                    body["reasoning"] = json!({ "effort": cfg.reasoning.as_str() });
                }
                // A local server's chat template takes it this way, and
                // nothing else knows the parameter at all.
                _ if !NO_KWARGS.load(Ordering::Relaxed) => {
                    body["chat_template_kwargs"] =
                        json!({ "enable_thinking": cfg.reasoning.thinks() });
                }
                _ => {}
            }
            if tools && round < TOOL_ROUNDS {
                let offered = tools_for(
                    allowed,
                    lobby,
                    cfg.search,
                    cue.deciding(),
                    hand_on,
                    art,
                    clones.is_some(),
                    stage.map(|s| s.presets.as_slice()),
                );
                body["tools"] = match api {
                    Api::Responses => flatten_tools(&offered),
                    _ => offered,
                };
                if api != Api::Responses && REASONING_OFF.load(Ordering::Relaxed) {
                    body["reasoning_effort"] = json!("none");
                }
            }
            match cfg.authorize(http.post(&url)).send_json(body) {
                Ok(r) => break (r, api),
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    // No /responses here: a local llama.cpp or vLLM. Said
                    // once for the process, then the room speaks chat.
                    if api == Api::Responses
                        && cfg.api == Api::Auto
                        && matches!(code, 404 | 405 | 501)
                    {
                        if !NO_RESPONSES.swap(true, Ordering::Relaxed) {
                            info!("{url} has no /responses; speaking chat/completions");
                        }
                        api = Api::Chat;
                        continue;
                    }
                    if code == 400
                        && api != Api::Responses
                        && !NO_KWARGS.load(Ordering::Relaxed)
                        && detail.contains("chat_template_kwargs")
                    {
                        NO_KWARGS.store(true, Ordering::Relaxed);
                        continue;
                    }
                    // Reasoning models refuse tools on /chat/completions
                    // unless the thinking is off. /responses is the way
                    // to have both, so say so rather than silently
                    // dropping either.
                    if code == 400
                        && api != Api::Responses
                        && tools
                        && detail.contains("reasoning_effort")
                        && !REASONING_OFF.swap(true, Ordering::Relaxed)
                    {
                        warn!(
                            "{url} won't take tools while {model} is thinking; \
                             turning thinking off (--llm-api responses keeps both)"
                        );
                        if cfg.reasoning.thinks() {
                            room.error(&format!(
                                "{model} won't think and use tools on this API; \
                                 run with --llm-api responses to have both."
                            ));
                        }
                        continue;
                    }
                    if code == 400 && tools && detail.contains("tool") {
                        warn!("{url} refuses tools, so games and cards won't work: {detail}");
                        if !TOOLS_REFUSED.swap(true, Ordering::Relaxed) {
                            room.error(&format!(
                                "{model} won't take tools here, so games and cards won't work: {}",
                                error_message(&detail)
                            ));
                        }
                        tools = false;
                        continue;
                    }
                    bail!(
                        "{url}: HTTP {code}: {}",
                        detail.chars().take(300).collect::<String>()
                    );
                }
                Err(e) => return Err(e).with_context(|| format!("POST {url}")),
            }
        };
        let ms = || asked.elapsed().as_millis() as u64;
        info!(g, round, after_asking_ms = ms(), "reply: stream opened");
        let mut pending = String::new();
        let mut said = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        for line in std::io::BufReader::new(resp.into_reader()).lines() {
            if generation.load(Ordering::SeqCst) != g {
                info!(g, "reply dropped");
                return Ok(());
            }
            let line = line.context("read reply stream")?;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let was_silent = said.is_empty();
            let over = fold_event(api, &v, &mut said, &mut pending, &mut calls);
            if was_silent && first && !said.trim().is_empty() {
                info!(g, after_asking_ms = ms(), "reply: first words");
            }
            if over {
                break;
            }
            while let Some(end) =
                sentence_end(&pending).or_else(|| first.then(|| clause_end(&pending)).flatten())
            {
                let s: String = pending.drain(..end).collect();
                if first {
                    info!(
                        g,
                        after_asking_ms = ms(),
                        chars = s.len(),
                        "reply: first sentence"
                    );
                    first = false;
                }
                send_sentence(tx, g, &s, &mut spoken);
            }
        }
        send_sentence(tx, g, &pending, &mut spoken);
        if !pending.trim().is_empty() {
            first = false;
        }
        calls.retain(|c| !c.name.is_empty());
        if calls.is_empty() {
            return Ok(());
        }
        // Having answered after one tool call, a model that calls another
        // tends to start its answer over in new words: run the calls (a
        // save still makes its card) but let this reply end here.
        let answered = round > 0 && !said.trim().is_empty();
        // The model's text so far stays in its turn, with the calls.
        push_turn(api, &mut convo, &said, &calls);
        let spoke = !spoken.is_empty();
        let mut results = Vec::new();
        for c in calls.iter() {
            // The room counts a reply that has gone quiet against the
            // turn, because normally the only thing that goes quiet
            // that long is a voice thread that died. A tool of ours is
            // the exception: an upload to the voice server can take
            // half a minute and an endpoint can be slow, and a turn
            // ended underneath it means whatever the model says about
            // the result is never heard. So the room is told.
            let working = room.working(g);
            let result = run_tool(room, cfg, g, c, allowed, art, clones, stage);
            drop(working);
            info!(g, tool = c.name, %result, "reply: tool");
            results.push(result);
        }
        // A tool that failed always goes back, answered or not. What it
        // said before the call is a promise about that tool -- "I'm
        // putting the passage up now" -- and a failure nobody told it
        // about is the room letting it make that promise to somebody
        // who is now waiting on nothing. Having one of them fail puts
        // every result back, because a call the model made with no
        // result against it is a turn the endpoint will reject.
        let failed = results.iter().any(|r| went_wrong(r));
        if !answered || failed {
            for (i, (c, mut result)) in calls.iter().zip(results).enumerate() {
                if spoke {
                    result.push_str(
                        "\n(What you said before this call has already been spoken aloud. \
                         Carry on from there; don't repeat it.)",
                    );
                }
                push_result(api, &mut convo, c, i, result);
            }
        }
        if room.is_starting(g) {
            info!(g, "reply: started a game, so it ends here");
            return Ok(());
        }
        // The curtain is up, so this reply is over whatever the model
        // had left to say. What it has already said is the
        // announcement, which is spoken; the play begins the moment it
        // stops speaking, and what it would say *after* a show belongs
        // to the turn it is given when the curtain comes down.
        //
        // Structural, and it has to be: the tool's answer says to stop
        // talking and the prompt says the same, and the room then
        // hands every tool result back with "carry on from there" on
        // it, which is the opposite instruction and the one nearer the
        // model. Asked to put a play on, it announced the play and
        // then said "that's it, how did that land?" -- before a word
        // of the play had been heard.
        if !failed && calls.iter().any(|c| c.name == "put_on_show") {
            info!(g, "reply: the curtain is up, so it ends here");
            return Ok(());
        }
        // It judged the pause not to be its own: nothing more to say, and
        // the room has already dropped whatever was queued.
        if calls.iter().any(|c| c.name == "stay_quiet") {
            info!(g, "reply: stayed quiet");
            return Ok(());
        }
        if generation.load(Ordering::SeqCst) != g {
            info!(g, "reply dropped");
            return Ok(());
        }
        if answered && !failed {
            info!(
                g,
                "reply: already answered, so not asking again after the tools"
            );
            return Ok(());
        }
        if answered {
            info!(g, "reply: a tool failed, so asking again to say so");
        }
    }
    Ok(())
}

#[derive(Default, Debug, Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCall {
    fn id(&self, i: usize) -> String {
        if self.id.is_empty() {
            format!("call_{i}")
        } else {
            self.id.clone()
        }
    }
}

/// The `error.message` of an OpenAI-style error body, else the body.
fn error_message(detail: &str) -> String {
    serde_json::from_str::<Value>(detail)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| detail.chars().take(300).collect())
}

/// How long to wait for a web search before giving up on it. Long enough
/// for a real search, short enough that the room isn't silent for ever.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap the answer: it gets read aloud, and the model has to speak it.
const SEARCH_TOKENS: u32 = 700;

/// A function tool, as the endpoint wants it.
fn tool(n: &str, d: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": n,
            "description": d,
            "parameters": { "type": "object", "properties": props, "required": required },
        }
    })
}

/// The tools the model has everywhere: who is here, the cards it can deal
/// them one at a time, and standing down.
fn room_tools() -> Vec<Value> {
    vec![
        tool(
            "list_people",
            "List who is in the room: their exact name, whether their \
             microphone is open, what cards they are holding, and what their \
             avatar was asked for if they have one. Only the people who are \
             actually here; somebody invited but elsewhere is not in this \
             room. Use it to get a name right before sending a card.",
            json!({}),
            &[],
        ),
        tool(
            "send_card",
            "Put a card in one person's cards, where only they can see it. \
             Use it for anything meant for them alone -- a secret role, a \
             hand, what only their character notices, an instruction for them \
             -- and never say aloud what is on it. One person per card: to \
             tell two people different things, send two cards. Sending a \
             title they already hold rewrites that card in place, which is \
             how you keep a hand or a standing order up to date.",
            json!({
                "to": { "type": "string", "description": "Exactly the name list_people gives for that person." },
                "title": { "type": "string", "description": "A few words naming the card, e.g. \"Your role\" or \"What you notice\"." },
                "body": { "type": "string", "description": "What that person alone is to read. Written to be read, not spoken." },
            }),
            &["to", "title", "body"],
        ),
        tool(
            "post_card",
            "Write a card into the conversation for the whole room to \
             read. Use it for anything the people will want in front of \
             them rather than held in their heads, and for everything \
             you would otherwise have to read out as a list of numbers: \
             a forecast, times, prices, options to choose between, the \
             standings, the rules you have just agreed, a map in words. \
             **Post it and say it at the same time** -- a card is not \
             instead of answering. Say the short spoken version in your \
             own voice (\"it's cold tomorrow and wet on Friday\") and put \
             the detail on the card, where it can be looked at. Nobody \
             hears a card appear, so say that it is there. It stays in \
             the conversation and cannot be taken back, so post what \
             will still be true.",
            json!({
                "title": { "type": "string", "description": "A few words heading the card, e.g. \"Boulder, next three days\" or \"Scores after round 2\"." },
                "body": { "type": "string", "description": "What the room is to read, in markdown: tables, bullet and numbered lists, **bold**, `code`, quotes, links. A table is the right shape for anything with columns, and a list for anything you would otherwise count out loud. Written to be read, not spoken." },
            }),
            &["title", "body"],
        ),
        tool(
            "take_back_card",
            "Take one card back: it leaves that person's cards at once. \
             Nobody in the room can clear a card themselves, so what each \
             of them is holding is yours to keep right -- take a card back \
             once it no longer applies.",
            json!({
                "to": { "type": "string", "description": "Exactly the name list_people gives for that person." },
                "title": { "type": "string", "description": "The title of the card to take back, as list_people gives it." },
            }),
            &["to", "title"],
        ),
        tool(
            "read_cards",
            "Read the cards you have dealt, in full. Use it when you need \
             what you told one person alone and it has fallen out of the \
             conversation you can still see -- who has which role, what is \
             in whose hand.",
            json!({
                "to": { "type": "string", "description": "One person's cards. Leave it out for everyone's." },
            }),
            &[],
        ),
        tool(
            "hand_to",
            "Put the turn to another AI voice in this room: it answers \
             next, as though somebody had said its name. Use it when you \
             have actually put something to it and want its answer -- \
             asked it a question, asked what it makes of something, \
             asked it to take this one. Not as a courtesy, and never to \
             pass on something that was asked of you. Say your line \
             first: what you say is spoken, and then it answers. The \
             room allows only a couple of these in a row with nobody \
             else saying anything, so spend them on something you want \
             an answer to.",
            json!({
                "bot": { "type": "string", "description": "Exactly the name of the other voice, as you have seen it in the conversation." },
            }),
            &["bot"],
        ),
        tool(
            "wait_until_named",
            "Stop answering, and say nothing at all until someone says your \
             name again. Call it when the people are done talking to you -- \
             they thank you, say goodbye, or turn to talking among themselves \
             -- so that you don't answer a conversation that isn't yours.",
            json!({}),
            &[],
        ),
    ]
}

/// Offered only when the room is leaving the AI to judge the moment.
fn quiet_tool() -> Value {
    tool(
        "stay_quiet",
        "Say nothing this turn: the pause you were shown is not yours. \
         Call it instead of writing anything at all -- then nothing of \
         yours is spoken and nothing appears on screen, as though the \
         room had never asked. Use it whenever the people are talking \
         with each other rather than with you.",
        json!({
            "ask_again_in_seconds": {
                "type": "integer",
                "minimum": 1,
                "maximum": 120,
                "description": "Ask to be shown the room again if nobody says anything for this many seconds -- how you wait for a real silence instead of the short gap you were just shown. Leave it out to simply stay out.",
            },
        }),
        &[],
    )
}

/// The `reply_mode` a game is saved with.
fn reply_mode_arg() -> Value {
    json!({
        "type": "string",
        "enum": ["room", "always", "named"],
        "description": "How the room runs while this game is played: \"always\" (you answer every pause, for a game you deal the turns in), \"named\" (you speak only when a player says your name), or \"room\" (leave it however the people have it set). Defaults to \"room\".",
    })
}

/// Offered in the lobby when `--search` is on, to every bot: looking
/// something up is reading, and reading is what a voice may do.
fn look_up_tool() -> Value {
    tool(
        "look_up",
        "Search the web and answer one question from what it finds. Use it \
         whenever the answer depends on the world as it is now or on \
         specifics you can't be sure of -- places, opening hours, prices, \
         events, news, people, anything recent. Prefer it over guessing. \
         Ask one plain question, with the place or date in it.",
        json!({ "question": {
            "type": "string",
            "description": "The question, in full, as you would ask a person: \"good restaurants in Boulder, Colorado\".",
        }}),
        &["question"],
    )
}

/// Everything the model may do this turn: the cards anywhere, and in the
/// lobby designing games and looking things up when `search` is on --
/// unless this bot is only [`Allowed::Looking`], and then it is who is
/// here, the web, and the two tools a turn is taken with.
#[allow(clippy::too_many_arguments)]
fn tools_for(
    allowed: Allowed,
    lobby: bool,
    search: bool,
    deciding: bool,
    hand_on: bool,
    art: &Art,
    cloning: bool,
    // The voices the speech server has, or None in a house that cannot
    // give a puppet a voice -- and then the puppet tools are not
    // offered at all. Some(&[]) is a house that can, whose operator
    // did not say which voices it holds.
    puppets: Option<&[String]>,
) -> Value {
    // Not offered when there is nobody to hand to, or nothing left to
    // hand: the model is told as much in `LAST_ROUND`, and a tool it
    // cannot use is a tool it will try.
    let offer = |t: &Value| hand_on || t["function"]["name"].as_str().unwrap_or("") != "hand_to";
    if allowed == Allowed::Looking {
        let mut out: Vec<Value> = room_tools()
            .into_iter()
            .filter(|t| allowed.may(t["function"]["name"].as_str().unwrap_or("")))
            .filter(&offer)
            .collect();
        if deciding {
            out.push(quiet_tool());
        }
        if lobby && search {
            out.push(look_up_tool());
        }
        return Value::Array(out);
    }
    let name =
        json!({ "type": "string", "description": "The game's name, as the players will see it." });
    let mut extra: Vec<Value> = room_tools().into_iter().filter(offer).collect();
    if deciding {
        extra.push(quiet_tool());
    }
    // Pictures everywhere, and not only in the lobby the way games
    // are. A portrait of somebody's character is as much a thing to
    // make in the middle of a game as before one, and a game whose
    // prompt doesn't want them simply never asks.
    //
    // The two face tools are here whether or not anything can draw: a
    // picture can be dropped into any room from somebody's own
    // computer, so a house with no picture server still has faces to
    // give and bios to write. Only the drawing needs a server.
    extra.push(avatar_tool());
    extra.push(bio_tool());
    // Everywhere, like the faces and for the same reason: somebody in
    // the middle of a game is as likely to want their own voice in it
    // as somebody who isn't. Only where there is a server to clone
    // with -- see `Agent::clones`.
    if cloning {
        extra.extend(voice_tools());
    }
    // Everywhere, like the faces and the voices: a play is as much a
    // thing to put on in the middle of a game as in the lobby, and a
    // game whose prompt doesn't want one never asks.
    if let Some(presets) = puppets {
        extra.extend(puppet_tools(cloning, presets));
    }
    if art.any() {
        for server in &art.servers {
            extra.extend(server.offer());
        }
    }
    if !lobby {
        return Value::Array(extra);
    }
    let game = json!({
        "name": name,
        "summary": { "type": "string", "description": "One or two sentences the players see on the game's button." },
        "prompt": { "type": "string", "description": "Your complete hidden instructions for running the game." },
        "reply_mode": reply_mode_arg(),
        "players_min": { "type": "integer", "minimum": 1, "maximum": 99, "description": "Fewest players the game works with. Leave out if any number will do." },
        "players_max": { "type": "integer", "minimum": 1, "maximum": 99, "description": "Most players the game works with. Leave out if there is no upper limit." },
    });
    let mut revise = game.clone();
    revise["new_name"] =
        json!({ "type": "string", "description": "A new name, if the game is being renamed." });
    let mut tools = json!([
        tool("list_games", "List the saved games: name, summary, and whether they have been played (and so are fixed).", json!({}), &[]),
        tool("check_game_name", "Check whether a name is free for a new game.", json!({ "name": name }), &["name"]),
        tool("save_game", "Save a new game once the players have confirmed it. Its button appears in the chat.", game, &["name", "summary", "prompt"]),
        tool("read_game", "Read the summary and hidden prompt of a saved game, to revise it.", json!({ "name": name }), &["name"]),
        tool("revise_game", "Replace a saved game with a revised version; its card in the chat is replaced by a new one.", revise, &["name", "summary", "prompt"]),
        tool("start_game", "Start playing a saved game: everyone moves into a fresh conversation run by its prompt.", json!({ "name": name }), &["name"]),
        tool("delete_game", "Delete a saved game for good, once the players have confirmed which one.", json!({ "name": name }), &["name"]),
    ]);
    tools
        .as_array_mut()
        .expect("tools is an array")
        .extend(extra);
    if search {
        tools
            .as_array_mut()
            .expect("tools is an array")
            .push(look_up_tool());
    }
    tools
}

/// Recording somebody reading aloud, keeping a voice from it, wearing
/// one, and getting rid of one.
///
/// Four tools rather than one that does everything, because each is a
/// different moment of a conversation with a person in it: the asking,
/// the naming, the wearing and the forgetting. A single tool with a
/// mode argument would be one the model could call in the wrong
/// order, and the wrong order here is a microphone recording somebody
/// who hasn't been told.
///
/// What the room does itself is in `Room::take_voice_sample`: the
/// passage card, the card that says it is listening, and the turn
/// handed back when the clip lands.
fn voice_tools() -> Vec<Value> {
    vec![
        tool(
            "take_voice_sample",
            "Record a few seconds of somebody in the room reading aloud, so a \
             voice can be cloned from it. Only when they have asked for it. \
             Their screen gets a card with the passage on it and the room \
             starts listening: the next thing they say is the clip. Say what \
             you are doing first, then call this and stop talking -- you are \
             given the turn again when the clip is in.",
            json!({
                "who": {
                    "type": "string",
                    "description": "Whose voice, as the room calls them.",
                },
                "passage": {
                    "type": "string",
                    "description": "What they are to read, written out in full: four or \
                                    five ordinary sentences, ten or twenty seconds of \
                                    speech. Not a word list, and nothing anybody would \
                                    rather not say out loud.",
                },
            }),
            &["who", "passage"],
        ),
        tool(
            "save_voice",
            "Keep a voice cloned from one of the clips read in this \
             conversation, under a name. Ask them what to call it first. From \
             then on it is the house's: any room can ask to speak in it.",
            json!({
                "sample": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The number on the clip's card in the chat.",
                },
                "name": {
                    "type": "string",
                    "description": "What the voice is called from now on, as they asked \
                                    for it: \"Ryan\", \"the old sailor\".",
                },
                "description": {
                    "type": "string",
                    "description": "A few words about how the voice sounds, if they said \
                                    anything about it. Leave it out otherwise.",
                },
            }),
            &["sample", "name"],
        ),
        tool(
            "list_voices",
            "The voices the house has cloned, and whose each one is.",
            json!({}),
            &[],
        ),
        tool(
            "speak_as",
            "Speak in one of the cloned voices instead of your own, from your \
             next sentence on. It lasts until somebody asks you to change \
             again. Only because somebody asked.",
            json!({ "voice": {
                "type": "string",
                "description": "The name of a cloned voice, or \"your own\" to go back \
                                to your own voice.",
            }}),
            &["voice"],
        ),
        tool(
            "forget_voice",
            "Delete a cloned voice for good, once they have said which one. \
             The clip it was made from stays in the conversation.",
            json!({ "voice": {
                "type": "string",
                "description": "The name of the voice to delete.",
            }}),
            &["voice"],
        ),
    ]
}

/// The puppets: keeping a character, putting it in the room, and
/// putting a show on.
///
/// Seven tools and not one with a mode, for the reason the voice tools
/// are four: each is a different moment of a conversation with people
/// in it -- the asking, the making, the casting, the performance, the
/// questions afterwards -- and a single tool with a mode argument is
/// one a model can call in the wrong order. The wrong order here is a
/// play nobody was asked about.
///
/// `cloning` says whether the house has a voice-cloning server, which
/// decides whether a puppet can be given somebody's real voice.
/// `presets` are the voices the speech server has, if the operator said
/// what they are: the room does not otherwise know, and a list it
/// invented would be a list of voices that do not exist.
fn puppet_tools(cloning: bool, presets: &[String]) -> Vec<Value> {
    let puppet = json!({
        "type": "string",
        "description": "The puppet's name, as it is called in the room.",
    });
    let mut voice = json!({
        "name": puppet,
        "about": {
            "type": "string",
            "description": "Who the character is, in a line or two: what you would \
                            tell an actor before they read. Kept with the puppet, so \
                            it can be cast again later.",
        },
        "delivery": {
            "type": "string",
            "description": "How it talks, written as instructions to an actor: \
                            \"gruff and slow, an old soldier who has told this story \
                            before\". This is what makes it sound like itself rather \
                            than like you, so write it properly.",
        },
        "preset": {
            "type": "string",
            "description": match presets.is_empty() {
                true => "One of the voices the speech server has, to build the \
                         character on. Leave it out for the room's own voice, which \
                         the delivery then shapes."
                    .to_string(),
                false => format!(
                    "One of the voices the speech server has, to build the character \
                     on: {}. Leave it out for the room's own voice, which the \
                     delivery then shapes.",
                    presets.join(", ")
                ),
            },
        },
    });
    if cloning {
        voice["voice"] = json!({
            "type": "string",
            "description": "A cloned voice, by the name it is kept under, to use \
                            instead of a preset: this is how a puppet speaks in the \
                            voice of somebody who read for it. list_voices says what \
                            there is.",
        });
    }
    let line = json!({
        "type": "object",
        "description": "One line of the play.",
        "properties": {
            "who": {
                "type": "string",
                "description": "Who says it: a puppet on stage, or your own name to \
                                take a part yourself.",
            },
            "say": {
                "type": "string",
                "description": "What they say, word for word, as it will be spoken, \
                                and all of it is spoken. No stage directions and no \
                                narration -- nothing happens in this play except what \
                                somebody says. To have a character interrupted, write \
                                the line as far as they get and end it with a dash: \
                                \"But I only wanted--\". That is the interruption; \
                                there is no other kind.",
            },
            "pause": {
                "type": "number",
                "minimum": 0,
                "description": "Seconds of silence before this line, measured from the \
                                moment the line before it was heard out. Leave it out \
                                for a natural beat; 0 for an answer that lands on the \
                                last word; 2 or 3 for a character taking their time.",
            },
            "over": {
                "type": "number",
                "minimum": 0,
                "description": "Come in over the end of the line before this one, this \
                                many seconds before it finishes, so both voices are \
                                heard at once for that long. Half a second is an \
                                interjection landing on somebody's last word. It cuts \
                                nothing off: pair it with a line written short to be \
                                interrupted. Leave it out on any line that waits its \
                                turn -- `pause` is how those are timed.",
            },
        },
        "required": ["who", "say"],
    });
    vec![
        tool(
            "list_puppets",
            "The puppets the house has kept, who is on stage in this room now, and \
             the voices the speech server has. Speech recognition mangles names, so \
             match what you hear to the closest one on this list.",
            json!({}),
            &[],
        ),
        tool(
            "make_puppet",
            "Keep a character under a name, with a voice of its own. Writing to a \
             name that already exists revises that puppet, which is how a character \
             is changed after the room has heard it. Only once the room has said who \
             it wants.",
            voice,
            &["name", "about"],
        ),
        tool(
            "forget_puppet",
            "Delete a puppet for good, with its face and the lines beside it, once \
             the room has said which one.",
            json!({ "puppet": puppet }),
            &["puppet"],
        ),
        tool(
            "bring_on_puppets",
            "Put puppets in the room: each joins as its own voice, with its name, \
             its face and a fader of its own, and everybody sees it arrive. Do this \
             before a show, so the room can see who is about to speak.",
            json!({ "puppets": {
                "type": "array",
                "items": { "type": "string" },
                "description": "The puppets to bring on, by name.",
            }}),
            &["puppets"],
        ),
        tool(
            "send_off_puppets",
            "Take puppets out of the room. What they said stays in the conversation. \
             The characters themselves are kept and can be cast again.",
            json!({ "puppets": {
                "type": "array",
                "items": { "type": "string" },
                "description": "The puppets to send away, by name.",
            }}),
            &["puppets"],
        ),
        tool(
            "put_on_show",
            "Play a whole scripted scene, now. Every line is turned into speech \
             before it is needed, so the lines land exactly where you put them -- \
             which is the only reason to do this rather than talk. Announce the show \
             first and then stop talking: it begins when you have finished speaking, \
             and while it plays you answer nothing. You are given the turn again when \
             the curtain comes down.",
            json!({
                "title": {
                    "type": "string",
                    "description": "What the show is called, for the room.",
                },
                "script": {
                    "type": "array",
                    "items": line,
                    "description": "Every line of the play, in order.",
                },
            }),
            &["title", "script"],
        ),
        tool(
            "say_as",
            "Say one line as one of the puppets, now, in its voice. For after a show, \
             when a puppet is still in the room and somebody has asked it something. \
             Say nothing in your own voice in the same turn.",
            json!({
                "who": puppet,
                "say": {
                    "type": "string",
                    "description": "What it says, word for word.",
                },
            }),
            &["who", "say"],
        ),
    ]
}

/// Writing the few lines shown beside somebody's face.
///
/// Its own tool rather than an argument to `set_avatar`: a bio is
/// revised by talking -- "say I'm from the north instead" -- long
/// after the picture is settled, and a tool that could only write one
/// alongside a new face would make people sit for another portrait to
/// fix a sentence.
fn bio_tool() -> Value {
    tool(
        "set_bio",
        "Write the few lines shown beside somebody's face when anyone clicks it: who \
         their character is, in two or three sentences, in the words they gave you. \
         Read it back to them and revise it if they want -- writing again replaces \
         it, and an empty bio takes it away. Write it from what they actually said, \
         not from what the picture looks like.",
        json!({
            "who": {
                "type": "string",
                "description": "Whose it is. Anybody here: a person in the room, \
                                yourself, or another AI in it.",
            },
            "bio": {
                "type": "string",
                "description": "The few lines, or empty to take theirs away.",
            },
        }),
        &["who", "bio"],
    )
}

/// Putting a picture that is already in the chat on somebody.
///
/// Deliberately two steps and not one. Making the picture is the
/// picture server's tool, with everything that server knows about
/// sizes and styles in its own schema; this is the room's half, and
/// all it decides is whose face it is. It is also the half that works
/// without a server at all, since a picture can be dropped into the
/// chat by whoever is in the room. A model with no picture to put is
/// told so when it calls this.
fn avatar_tool() -> Value {
    tool(
        "set_avatar",
        "Put one of the pictures in the chat on somebody: it becomes the square shown \
         beside everything they say, to everyone. Only do this once they have seen it \
         and said they are happy with it -- every picture in the chat, whether you made \
         it or somebody dropped it in, is already there for them to look at, so there \
         is nothing to settle before they have.",
        json!({
            "who": {
                "type": "string",
                "description": "Whose face it is. Anybody here: a person in the room, \
                                yourself, or another AI in it. A picture is not made for \
                                anyone in particular, so the same one can go to whoever \
                                the room decides it suits.",
            },
            "picture": {
                "type": "integer",
                "minimum": 1,
                "description": "The number on the picture's card in the chat. They are \
                                numbered from 1 in the order they turned up, so an \
                                earlier one can still be chosen.",
            },
        }),
        &["who", "picture"],
    )
}

/// Answer one question with a web search, letting the endpoint do the
/// searching (OpenAI's `/responses` with its hosted `web_search` tool).
/// Not streamed: the model is waiting on the result, and the room shows
/// it as thinking until it comes back.
fn look_up(cfg: &Chat, question: &str) -> Result<String> {
    // Not the shared agent: that one waits 120s for the model to stream,
    // and a search that hung for 120s would leave the room silent.
    let http = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(SEARCH_TIMEOUT)
        .build();
    let url = cfg.endpoint("responses");
    let body = json!({
        "model": cfg.model,
        "input": question,
        "tools": [{ "type": "web_search" }],
        "max_output_tokens": SEARCH_TOKENS,
    });
    let resp = match cfg.authorize(http.post(&url)).send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let detail = r.into_string().unwrap_or_default();
            bail!(
                "{url}: HTTP {code}: {}",
                detail.chars().take(200).collect::<String>()
            );
        }
        Err(e) => return Err(e).with_context(|| format!("POST {url}")),
    };
    let v: Value = resp.into_json().context("read the search result")?;
    let text = search_text(&v);
    if text.trim().is_empty() {
        bail!("the search came back empty");
    }
    Ok(text)
}

/// The answer out of a `/responses` body: the `output_text` of its
/// message items, and nothing else.
///
/// `/responses` returns a list of output items -- `reasoning`,
/// `web_search_call`, `message`. Only the message is the answer. The
/// reasoning item carries a `content` array of its own, empty in
/// practice but not by contract, and whatever the model thought on its
/// way to an answer must never reach the voice.
fn search_text(v: &Value) -> String {
    v["output"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|i| i["type"] == "message")
                .flat_map(|i| i["content"].as_array().into_iter().flatten())
                .filter(|c| c["type"] == "output_text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// A tool call that went wrong, as the room writes one down: every
/// refusal from `run_tool` is an `error` and nothing else is.
fn went_wrong(result: &str) -> bool {
    serde_json::from_str::<Value>(result).is_ok_and(|v| !v["error"].is_null())
}

/// Run one tool call; the result is JSON for the model.
#[allow(clippy::too_many_arguments)]
fn run_tool(
    room: &Room,
    cfg: &Chat,
    g: u64,
    call: &ToolCall,
    allowed: Allowed,
    art: &Art,
    clones: Option<&crate::voices::Clones>,
    stage: Option<&Stage>,
) -> String {
    // It was never offered this one, which does not stop a model asking
    // for it. The room's answer is the same either way: there is no
    // such tool for you.
    if !allowed.may(&call.name) {
        info!(g, tool = %call.name, "reply: refused a tool this bot hasn't got");
        return json!({
            "error": format!("you have no tool called {:?}", call.name),
        })
        .to_string();
    }
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let arg = |k: &str| args[k].as_str().unwrap_or("").to_string();
    // An unknown reply_mode would quietly become the default, so say so.
    let settings = || {
        let reply = match args["reply_mode"].as_str() {
            None => ReplyMode::default(),
            Some(m) => ReplyMode::parse(m).ok_or_else(|| {
                format!("reply_mode is \"room\", \"auto\", \"always\" or \"named\", not {m:?}")
            })?,
        };
        let players = Players {
            min: args["players_min"].as_u64().map(|n| n as u32),
            max: args["players_max"].as_u64().map(|n| n as u32),
        }
        .check()?;
        Ok(GameSettings { reply, players })
    };
    let result = match call.name.as_str() {
        "list_games" => Ok(json!({ "games": room.list_games() })),
        "check_game_name" => room
            .check_game_name(&arg("name"))
            .map(|()| json!({ "free": true })),
        "save_game" => settings().and_then(|s| {
            room.save_game(g, &arg("name"), &arg("summary"), &arg("prompt"), s)
                .map(|()| json!({ "saved": arg("name").trim() }))
        }),
        "read_game" => room.read_game(&arg("name")).map(|(summary, s, prompt)| {
            json!({
                "summary": summary,
                "reply_mode": s.reply.as_str(),
                "players": s.players.describe(),
                "prompt": prompt,
            })
        }),
        "revise_game" => settings().and_then(|s| {
            room.revise_game(
                g,
                &arg("name"),
                args["new_name"].as_str(),
                &arg("summary"),
                &arg("prompt"),
                s,
            )
            .map(|name| json!({ "saved": name }))
        }),
        "start_game" => room
            .start_game(g, &arg("name"))
            .map(|name| json!({ "started": name })),
        "delete_game" => room
            .remove_game(g, &arg("name"))
            .map(|name| json!({ "deleted": name })),
        "list_people" => Ok(json!({ "people": room.people_here() })),
        "stay_quiet" => room
            .stay_quiet(g, args["ask_again_in_seconds"].as_u64())
            .map(|()| json!({ "quiet": true })),
        "hand_to" => room
            .hand_to(g, &arg("bot"))
            .map(|to| json!({ "handed_to": to })),
        "wait_until_named" => {
            // What the room is doing afterwards: a game that answers at
            // every pause still does, and the model should know.
            let mode = room.wait_until_named(g);
            Ok(json!({ "answering": mode }))
        }
        "send_card" => room
            .send_card(g, &arg("to"), &arg("title"), &arg("body"))
            .map(|to| json!({ "sent": true, "to": to })),
        "post_card" => room
            .post_card(g, &arg("title"), &arg("body"))
            .map(|title| json!({ "posted": title })),
        "take_back_card" => room
            .take_back_card(&arg("to"), &arg("title"))
            .map(|title| json!({ "taken_back": title })),
        "read_cards" => Ok(json!({ "people": room.read_cards(args["to"].as_str()) })),
        "look_up" => {
            let question = arg("question");
            info!(g, %question, "reply: looking it up");
            match look_up(cfg, &question) {
                Ok(answer) => Ok(json!({ "answer": answer })),
                Err(err) => {
                    // The model is told, so it can say it couldn't check
                    // rather than inventing an answer.
                    warn!(g, "look_up failed: {err:#}");
                    Err(format!(
                        "the search failed ({err}); say you couldn't look it up, and don't guess"
                    ))
                }
            }
        }
        "set_avatar" => match args["picture"].as_u64() {
            Some(n) => room
                .set_avatar(g, &arg("who"), n as u32)
                .map(|who| json!({ "set": who, "picture": n })),
            None => Err("which picture? `picture` is the number on its card in the \
                         chat, counting from 1"
                .to_string()),
        },
        "set_bio" => room
            .set_bio(&arg("who"), &arg("bio"))
            .map(|who| json!({ "wrote": who })),
        // Somebody's voice: recording them, keeping it, wearing it.
        // The room does the recording and the cards; the server work
        // is `cloning` below, beside the drawing for the same reason.
        "take_voice_sample" => room
            .take_voice_sample(g, &arg("who"), &arg("passage"))
            .map(|(n, who)| {
                json!({
                    "listening": true,
                    "sample": n,
                    "who": who,
                    "next": format!(
                        "{who} has the passage on their screen and the room is \
                         recording. Say nothing more until you are told the clip \
                         is in -- anything you say now is said over it."
                    ),
                })
            }),
        "save_voice" => match clones {
            Some(clones) => save_voice(room, clones, g, &args),
            None => Err(no_clone_server()),
        },
        "list_voices" => Ok(list_voices(room, clones)),
        "speak_as" => {
            let asked = arg("voice");
            room.speak_as(g, (!own_voice(&asked)).then_some(asked.as_str()))
                .map(|voice| json!({ "speaking_as": voice }))
        }
        "forget_voice" => match clones {
            Some(clones) => forget_voice(room, clones, &arg("voice")),
            None => Err(no_clone_server()),
        },
        // The puppets. All of them go through `stage`, which is None
        // in a house with nothing to speak with -- and then they were
        // never offered, so this is a model calling what it was not
        // shown.
        "list_puppets" | "make_puppet" | "forget_puppet" | "bring_on_puppets"
        | "send_off_puppets" | "put_on_show" | "say_as" => match stage {
            Some(stage) => puppeteering(room, stage, &call.name, &args),
            None => Err(
                "this house has no speech server, so a puppet would have no \
                         voice and there are no puppets here at all"
                    .to_string(),
            ),
        },
        // Somebody else's tool: one of the MCP servers the room lends,
        // under the prefixed name it was offered as. `Allowed` has
        // already had its say -- a bot that is only looking never got
        // offered these and is refused above.
        other => match art.whose(other) {
            Some(server) => {
                info!(g, tool = other, server = server.name(), "reply: over MCP");
                drawing(room, g, server, other, &args)
            }
            None => Err(format!("there is no tool called {other:?}")),
        },
    };
    match result {
        Ok(v) => v.to_string(),
        Err(e) => json!({ "error": e }).to_string(),
    }
}

/// The seven puppet tools, all in one place because they are one
/// activity: the roster, the stage and the show. Everything that
/// touches the room is the room's own method; what is here is reading
/// what the model sent and saying plainly what is wrong with it.
fn puppeteering(room: &Room, stage: &Stage, tool: &str, args: &Value) -> Result<Value, String> {
    let arg = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
    // A list of names, as the model sent it. One string is taken for a
    // list of one: a model that wrote `"puppets": "Grimble"` meant the
    // thing it plainly meant.
    let names = |k: &str| -> Result<Vec<String>, String> {
        match &args[k] {
            Value::Array(a) => {
                let out: Vec<String> = a
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                match out.is_empty() {
                    true => Err(format!("`{k}` is a list of puppet names, and it is empty")),
                    false => Ok(out),
                }
            }
            Value::String(s) if !s.trim().is_empty() => Ok(vec![s.trim().to_string()]),
            _ => Err(format!("`{k}` is a list of puppet names")),
        }
    };
    match tool {
        "list_puppets" => Ok(json!({
            "puppets": room.puppet_roster(),
            "on_stage": room.on_stage(),
            "voices": stage.presets,
            "show": room.show_title(),
        })),
        "make_puppet" => {
            let part = crate::store::Puppet {
                name: arg("name"),
                preset: stage.preset(&arg("preset"))?,
                delivery: arg("delivery"),
                clone: arg("voice"),
                about: arg("about"),
                created: 0,
            };
            room.make_puppet(part).map(|name| json!({ "kept": name }))
        }
        "forget_puppet" => room
            .forget_puppet(&arg("puppet"))
            .map(|name| json!({ "deleted": name })),
        // One answer for the lot, so a cast of three that was two
        // puppets and a typo says which was which rather than failing
        // the call and leaving two of them standing in the room.
        "bring_on_puppets" | "send_off_puppets" => {
            let on = tool == "bring_on_puppets";
            let mut done = Vec::new();
            let mut refused = Vec::new();
            for name in names("puppets")? {
                let r = match on {
                    true => room.cast_puppet(&name),
                    false => room.dismiss_puppet(&name),
                };
                match r {
                    Ok(name) => done.push(name),
                    Err(why) => refused.push(json!({ "puppet": name, "why": why })),
                }
            }
            if done.is_empty() {
                return Err(match refused.len() {
                    1 => refused[0]["why"].as_str().unwrap_or("no").to_string(),
                    _ => format!(
                        "none of them: {}",
                        refused
                            .iter()
                            .map(|r| format!(
                                "{} ({})",
                                r["puppet"].as_str().unwrap_or("?"),
                                r["why"].as_str().unwrap_or("no")
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                });
            }
            let mut out = json!({ if on { "on_stage" } else { "sent_off" }: done });
            if !refused.is_empty() {
                out["refused"] = Value::Array(refused);
            }
            Ok(out)
        }
        "put_on_show" => {
            // Who may have a line: this AI and the puppets standing in
            // the room. Not another bot -- that one answers for
            // itself.
            let director = room.config().bot().name.clone();
            let mut may_say = vec![director.clone()];
            may_say.extend(room.on_stage());
            if may_say.len() == 1 {
                return Err("there are no puppets in the room, so there is nobody to \
                            act: bring_on_puppets first"
                    .to_string());
            }
            // Logged when it is refused, and only then: a script the
            // room would not take is the one thing nobody can see
            // afterwards -- the model is told why and writes another,
            // and what it actually sent is gone.
            let script =
                match crate::puppet::Script::parse(&arg("title"), &args["script"], &may_say) {
                    Ok(script) => script,
                    Err(why) => {
                        info!(%why, script = %args["script"], "put_on_show: refused a script");
                        return Err(why);
                    }
                };
            let lines = script.lines.len();
            let title = script.title.clone();
            // The settings first, then the script: the curtain going
            // up is what stops the room answering, and a play that
            // began before it would be interrupted by its own
            // audience.
            room.begin_show(&director, &title, lines)?;
            match stage.hands().show(script) {
                Ok(()) => Ok(json!({
                    "playing": title,
                    "lines": lines,
                    "next": "Stop talking now. The play begins when you have finished \
                             speaking, and you are given the turn again when the \
                             curtain comes down.",
                })),
                // It never reached the thread, so there is no show to
                // end and nothing to put back but what was just taken.
                Err(why) => {
                    room.end_show();
                    Err(why)
                }
            }
        }
        "say_as" => {
            let who = arg("who");
            let say = arg("say");
            if say.is_empty() {
                return Err("say what it says".into());
            }
            if say.chars().count() > crate::puppet::LINE_MAX {
                return Err(format!(
                    "a line is at most {} characters; say it in a few of them and \
                     call this again",
                    crate::puppet::LINE_MAX
                ));
            }
            let on = room.on_stage();
            let Some(who) = on.iter().find(|p| p.eq_ignore_ascii_case(&who)) else {
                return Err(match on.is_empty() {
                    true => "there are no puppets in the room: bring_on_puppets first".to_string(),
                    false => format!(
                        "there is no puppet called {who:?} in the room; it is {}",
                        crate::room::and_list(&on)
                    ),
                });
            };
            stage.hands().say(who, &say).map(|()| {
                json!({
                    "said": true,
                    "who": who,
                    "next": format!(
                        "{who} is saying that. Say nothing in your own voice this turn."
                    ),
                })
            })
        }
        other => Err(format!("there is no tool called {other:?}")),
    }
}

/// What the model is told when it asks for a voice tool in a house
/// with no server to clone with. It is never offered them, so this is
/// only ever a model calling something it was not shown.
fn no_clone_server() -> String {
    "this house has no voice-cloning server, so there are no cloned voices \
     here at all"
        .to_string()
}

/// Whether what the model asked to speak in is its own voice again,
/// however it said it. The tool's description says "your own", and a
/// model that said "default" or "normal" meant the same thing.
fn own_voice(asked: &str) -> bool {
    matches!(
        asked.trim().to_ascii_lowercase().trim_end_matches('.'),
        "" | "own" | "your own" | "my own" | "yours" | "mine" | "itself"
            | "default" | "normal" | "none" | "your own voice" | "my own voice"
    )
}

/// Keep a voice cloned from a clip somebody read: the clip to the
/// server, the row to the house.
///
/// The upload is the slow half -- a few seconds of audio and the
/// server pulling an embedding off it -- and it happens here on the
/// agent's own thread, beside the model call and the pictures, which
/// is where every other wait in a reply already is.
fn save_voice(
    room: &Room,
    clones: &crate::voices::Clones,
    g: u64,
    args: &Value,
) -> Result<Value, String> {
    let Some(n) = args["sample"].as_u64() else {
        return Err("which clip? `sample` is the number on its card in the chat, \
                    counting from 1"
            .to_string());
    };
    let name = args["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Err("a voice needs a name: ask them what to call it".into());
    }
    if name.chars().count() > VOICE_NAME_MAX {
        return Err(format!("a voice's name is at most {VOICE_NAME_MAX} characters"));
    }
    let Some(id) = crate::voices::id_for(&name) else {
        return Err(format!(
            "there is nothing in {name:?} a server can keep a voice under; \
             ask them for a name with letters in it"
        ));
    };
    // A name that is already somebody else's voice, spelled
    // differently. Saving over it would rename the voice they have,
    // which is not what anybody asked for.
    if let Some(have) = room.voices().into_iter().find(|v| v.id == id) {
        if !have.name.eq_ignore_ascii_case(&name) {
            return Err(format!(
                "that is too close to {:?}, which is already {}'s voice; \
                 ask them for a different name",
                have.name, have.who
            ));
        }
    }
    let clip = room.voice_clip(n as u32)?;
    let wav = clip.wav.as_deref().unwrap_or_default();
    // What the clip says, for the server to clone in context. The
    // transcript where there is one, and what they were given to read
    // where the room never wrote one down.
    let said = match clip.said.trim() {
        "" => clip.passage.trim(),
        said => said,
    };
    // The server asks for a consent record, and this is the whole of
    // what the house knows about why the audio is allowed to be here.
    // For a reading that is a good deal: they were asked out loud, in
    // a room where everybody could see the light on, and they read it.
    // For a clip somebody brought in it is almost nothing -- who
    // dropped the file -- and the record says so rather than quietly
    // reading like the other kind.
    let consent = match clip.brought {
        false => format!("vox_room:{}:{}", room.config().id, clip.who),
        true => format!("vox_room:{}:{}:brought-in", room.config().id, clip.who),
    };
    // Replacing one of their own -- a second go at a voice that didn't
    // sound like them -- takes the first off the server, so what is
    // kept under the name is what was just read.
    if room.voices().iter().any(|v| v.id == id) {
        if let Err(err) = clones.forget(&id) {
            warn!(g, voice = %id, "couldn't take the old voice off the server: {err:#}");
        }
    }
    let description = args["description"].as_str().unwrap_or("");
    info!(
        g,
        voice = %id,
        who = %clip.who,
        clip = %clip.id,
        secs = format!("{:.1}", clip.secs),
        "keeping a cloned voice"
    );
    if let Err(err) = clones.upload(&id, wav, said, &consent, description) {
        warn!(g, voice = %id, "the clone server refused it: {err:#}");
        return Err(format!(
            "the voice server wouldn't keep it ({err}); say you couldn't, and \
             don't pretend the voice is there"
        ));
    }
    room.remember_voice(&id, &name, &clip.who, said)?;
    Ok(json!({
        "saved": name,
        "sample": clip.n,
        "cloned_from": clip.who,
        "seconds": format!("{:.0}", clip.secs),
        "next": format!(
            "{name} is kept. Tell them, and ask whether they want to hear it: \
             speak_as puts it on from your next sentence."
        ),
    }))
}

/// The voices the house has, and whether the server still has them.
///
/// Asked of the server as well as of the house because the two can
/// come apart -- a server rebuilt without its speakers directory, a
/// voice deleted from under it -- and a voice the AI offers and then
/// cannot speak in is worse than one it says has gone. A server that
/// cannot be reached is not reported as a server that has lost
/// everything: the list is the house's either way.
fn list_voices(room: &Room, clones: Option<&crate::voices::Clones>) -> Value {
    let held = clones.and_then(|c| match c.list() {
        Ok(names) => Some(names),
        Err(err) => {
            warn!("couldn't ask the voice server what it has: {err:#}");
            None
        }
    });
    json!({
        "voices": room
            .voices()
            .iter()
            .map(|v| {
                let mut out = json!({ "name": v.name, "cloned_from": v.who });
                if held.as_ref().is_some_and(|h| !h.contains(&v.id)) {
                    out["gone"] = json!(
                        "the voice server hasn't got this one any more, so it \
                         cannot be spoken in; forget_voice is the tidy answer"
                    );
                }
                out
            })
            .collect::<Vec<_>>(),
    })
}

/// Delete a cloned voice: the server's copy first, then the house's
/// row. That way round because a row for a voice the server has lost
/// is a voice the AI will try to wear and go silent in, while a copy
/// on the server that nothing names is only a few kilobytes nobody
/// can ask for.
fn forget_voice(
    room: &Room,
    clones: &crate::voices::Clones,
    asked: &str,
) -> Result<Value, String> {
    let voice = room.voice_named(asked)?;
    if let Err(err) = clones.forget(&voice.id) {
        return Err(format!(
            "the voice server wouldn't let go of it ({err}); it is still there"
        ));
    }
    room.forget_voice(&voice.id)?;
    Ok(json!({ "deleted": voice.name }))
}

/// One MCP call, with the room watching.
///
/// A call that is going to draw something puts a card into the
/// conversation *first*, with a spinner on it, and fills it in when
/// the picture lands. Generating one takes the better part of a
/// minute, and a room watching nothing happen is a room that asks
/// again -- so the card is the evidence that the AI is doing what it
/// just said it would.
///
/// Whether a call is going to draw is guessed from its arguments:
/// whether it was given any words to draw from -- see
/// [`mcp::asked_for`] -- which every image tool takes under one name
/// or another and none of the tools that merely report -- balances,
/// style lists, what was made earlier -- has. The guess is allowed to
/// be wrong in both directions and costs nothing either way. Guessed
/// yes and nothing was drawn: the card says the picture didn't come,
/// which is true. Guessed no and something was drawn: the pictures
/// are posted when the call returns, without the spinner anyone would
/// have watched.
///
/// This is where the room's half of the flow lives rather than the
/// model's, because the model cannot post a card and then make a
/// picture in one tool call -- and asking it to do two in the right
/// order, every time, before the thing people are waiting on, is a
/// rule that holds right up until it doesn't.
fn drawing(
    room: &Room,
    g: u64,
    server: &mcp::Server,
    tool: &str,
    args: &Value,
) -> Result<Value, String> {
    let prompt = mcp::asked_for(args);
    // The spinner, when this looks like a call that will draw.
    let pending = (!prompt.is_empty())
        .then(|| room.begin_picture(g, &prompt))
        .transpose()?;

    let outcome = match server.call(tool, args) {
        Ok(outcome) => outcome,
        Err(err) => {
            warn!(g, tool, "MCP call failed: {err:#}");
            if let Some((id, _)) = &pending {
                room.fail_picture(id, &format!("{err}"));
            }
            // Said plainly, so the model tells the room it couldn't
            // rather than describing a picture that was never made.
            return Err(format!(
                "{tool} failed ({err}); say you couldn't make it, and don't pretend you did"
            ));
        }
    };

    let mut drawn: Vec<u32> = Vec::new();
    let mut pictures = outcome.pictures.into_iter();
    // The first one fills the card that is already up; the rest get
    // cards of their own, since a call asked for several takes is a
    // call whose takes are each worth looking at.
    // What the tool said goes on every card it made, because the
    // handle for changing a picture is in there and the model will
    // have forgotten it by the time anybody asks: a tool result lives
    // in the one reply it arrived in, and the change is always asked
    // for in a later one. See `Entry::note`.
    if let Some((id, n)) = &pending {
        match pictures.next() {
            Some(first) => {
                room.finish_picture(id, &first.png, &outcome.said);
                drawn.push(*n);
            }
            None => room.fail_picture(id, "the tool came back without a picture"),
        }
    }
    for picture in pictures {
        let caption = if picture.prompt.trim().is_empty() {
            prompt.clone()
        } else {
            picture.prompt.clone()
        };
        match room.begin_picture(g, &caption) {
            Ok((id, n)) => {
                room.finish_picture(&id, &picture.png, &outcome.said);
                drawn.push(n);
            }
            Err(err) => warn!(g, "couldn't post a picture: {err}"),
        }
    }

    if drawn.is_empty() {
        return Ok(json!({ "result": outcome.said }));
    }
    // Deliberately not the pictures themselves: a 128x128 PNG is some
    // 30 KB of base64, and putting that through a conversation that is
    // read aloud helps nobody. The numbers are what the room can see
    // and what `set_avatar` takes.
    let numbers = drawn
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let many = drawn.len() > 1;
    Ok(json!({
        "result": format!(
            "Picture{} {numbers} {} now in the chat where everyone can see {}. \
             Say so, and ask how they like it. Do not call set_avatar until they \
             have said they are happy with one -- and if they want a change, make \
             another: it gets the next number, and an earlier one can still be chosen.{}",
            if many { "s" } else { "" },
            if many { "are" } else { "is" },
            if many { "them" } else { "it" },
            if outcome.said.is_empty() {
                String::new()
            } else {
                format!(" The tool also said: {}", outcome.said)
            }
        )
    }))
}

/// Hand a sentence to the voice, unless it repeats one this reply has
/// already said (models tend to start over after a tool call).
fn send_sentence(tx: &Voices, g: u64, s: &str, spoken: &mut Vec<Vec<String>>) {
    let s = clean(s);
    if s.is_empty() {
        return;
    }
    let w = sentence_words(&s);
    if spoken.iter().any(|old| repeats(old, &w)) {
        info!(g, sentence = %s, "reply: dropped a repeated sentence");
        return;
    }
    spoken.push(w);
    tx.send(Speak::Sentence(g, s));
}

fn sentence_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Shortest sentence that counts as a repeat: short ones ("Sure.") recur
/// for real.
const REPEAT_MIN_WORDS: usize = 5;
/// A shorter one said again word for word ("Quick Draw is available.").
const EXACT_REPEAT_MIN_WORDS: usize = 3;

/// `b` says nearly the same as `a`: most of the longer one's words shared.
fn repeats(a: &[String], b: &[String]) -> bool {
    if a == b && a.len() >= EXACT_REPEAT_MIN_WORDS {
        return true;
    }
    if a.len().min(b.len()) < REPEAT_MIN_WORDS {
        return false;
    }
    let mut rest: Vec<&String> = a.iter().collect();
    let mut shared = 0;
    for w in b {
        if let Some(i) = rest.iter().position(|r| *r == w) {
            rest.swap_remove(i);
            shared += 1;
        }
    }
    shared * 4 >= a.len().max(b.len()) * 3
}

/// Strip markdown that shouldn't be read aloud.
fn clean(s: &str) -> String {
    s.replace(['*', '#', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::BotConfig;
    use crate::room::Mode;

    #[test]
    fn drops_a_sentence_said_again_after_a_tool_call() {
        let (sender, rx) = crossbeam_channel::unbounded();
        let tx = Voices(vec![sender]);
        let mut spoken = Vec::new();
        for s in [
            "Sure.",
            "One idea is Quick Draw: I give you a category, and we take turns naming something.",
            "Sure.",
            "One idea is Quick Draw: I give you a category and we take turns naming things.",
            "Does that sound fun, or would you rather play something else?",
            "Quick Draw is available.",
            "Quick Draw is available.",
        ] {
            send_sentence(&tx, 1, s, &mut spoken);
        }
        drop(tx);
        let said: Vec<String> = rx
            .iter()
            .filter_map(|m| match m {
                Speak::Sentence(_, s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(said.len(), 5, "{said:?}");
        assert_eq!(said[2], "Sure.", "short sentences may recur");
    }

    fn tool_names(lobby: bool, search: bool) -> Vec<String> {
        tool_names_deciding(lobby, search, false)
    }

    fn tool_names_deciding(lobby: bool, search: bool, deciding: bool) -> Vec<String> {
        tools_for(
            Allowed::Everything,
            lobby,
            search,
            deciding,
            true,
            &Art::default(),
            false,
            None,
        )
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
    }

    /// The system prompt a turn goes out with.
    fn system_of(agent: &Agent, t: &Turn) -> String {
        let messages = agent.messages(t, None);
        messages[0]["content"].as_str().unwrap().to_string()
    }

    /// What is offered in the lobby, with the web, to a bot judging a
    /// pause -- `hand_on` being the only thing in question.
    fn tools_names(allowed: Allowed, hand_on: bool) -> Vec<String> {
        tools_for(
            allowed,
            true,
            true,
            true,
            hand_on,
            &Art::default(),
            false,
            None,
        )
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
    }

    fn looking_names(lobby: bool, search: bool, deciding: bool) -> Vec<String> {
        tools_for(
            Allowed::Looking,
            lobby,
            search,
            deciding,
            true,
            &Art::default(),
            false,
            None,
        )
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
    }

    /// A second AI in the room is a voice in it: it can see who is here
    /// and look something up, and the room's own tools are the first
    /// one's. The two it keeps besides are not powers but how a turn is
    /// given back.
    #[test]
    fn a_voice_in_the_room_has_only_the_tools_of_one() {
        assert_eq!(
            looking_names(true, true, true),
            [
                "list_people",
                "post_card",
                "hand_to",
                "wait_until_named",
                "stay_quiet",
                "look_up"
            ]
        );
        // Nothing that changes anything: no cards, and no games.
        let everything = tool_names_deciding(true, true, true);
        for power in [
            "send_card",
            "take_back_card",
            "read_cards",
            "save_game",
            "revise_game",
            "start_game",
            "delete_game",
            "list_games",
            "read_game",
            "check_game_name",
        ] {
            let looking = looking_names(true, true, true);
            assert!(everything.contains(&power.to_string()), "{power}");
            assert!(!looking.contains(&power.to_string()), "{power}");
        }

        // The web only where anybody has it: in the lobby, with
        // --search on. Judging is the only time stay_quiet is offered,
        // to either of them. And handing the turn on is offered only
        // while there is a turn left to hand on, so a bot never finds
        // out by being refused.
        assert!(!looking_names(true, false, true).contains(&"look_up".to_string()));
        assert!(!looking_names(false, true, true).contains(&"look_up".to_string()));
        assert!(!looking_names(true, true, false).contains(&"stay_quiet".to_string()));
        for names in [
            looking_names(true, true, true),
            tool_names_deciding(true, true, true),
        ] {
            assert!(names.contains(&"hand_to".to_string()));
        }
        for names in [
            tools_names(Allowed::Looking, false),
            tools_names(Allowed::Everything, false),
        ] {
            assert!(!names.contains(&"hand_to".to_string()), "{names:?}");
        }

        // And what is offered is what is allowed: a model that calls
        // something it was never shown is told there is no such tool.
        assert!(Allowed::Looking.may("list_people"));
        // Writing something for the room to look at is saying it, so
        // the voice that may only look may still post.
        assert!(Allowed::Looking.may("post_card"));
        assert!(
            !Allowed::Looking.may("send_card"),
            "a card to one person is not"
        );
        assert!(Allowed::Everything.may("send_card"));
    }

    /// What a bot is told it can do is what it can do. A note about
    /// dealing cards, to a voice that cannot deal one, is an
    /// instruction to try.
    #[test]
    fn a_voice_in_the_room_is_not_told_about_cards_or_games() {
        let (mut agent, t) = turn_with(Cue::Speak, &["Ryan"], false);
        agent.allowed = Allowed::Looking;
        let system = system_of(&agent, &t);
        assert!(!system.contains("send_card"), "{system}");
        assert!(!system.contains("save_game"));
        assert!(
            system.contains("wait_until_named"),
            "it still gives the turn back"
        );
        assert!(
            system.contains("post_card"),
            "and can still put something on the screen"
        );
        assert!(system.contains("in markdown"), "{system}");

        // In a game, the same: it plays, it doesn't deal.
        let frame = game_prompt("Dragon Heist", "Run a heist.", Allowed::Looking);
        assert!(!frame.contains("send_card"), "{frame}");
        assert!(frame.contains("wait_until_named"));
        assert!(frame.contains("post_card"), "it may still post one");
        let dealing = game_prompt("Dragon Heist", "Run a heist.", Allowed::Everything);
        assert!(dealing.contains("send_card"));
    }

    /// A house with no picture server is still told about faces, and
    /// not about making one. Nothing here can draw -- `Art::default()`
    /// -- and a picture can still arrive, dropped in by whoever is in
    /// the room, so `set_avatar` has something it may be asked to put.
    #[test]
    fn a_room_that_cannot_draw_is_still_told_about_faces() {
        let (agent, t) = turn_with(Cue::Speak, &["Ryan"], false);
        let system = system_of(&agent, &t);
        assert!(
            system.contains("set_avatar puts one of the pictures"),
            "{system}"
        );
        assert!(
            system.contains("drop one into the chat from their own computer"),
            "and where one comes from when nothing here draws: {system}"
        );
        assert!(system.contains("set_bio writes the few lines"), "{system}");
        // And not a word about doing what it cannot do.
        assert!(
            !system.contains("Each picture costs the house real money"),
            "{system}"
        );
        assert!(!system.contains("make it with a picture tool"), "{system}");
    }

    /// The other AIs in the room are in the transcript by name, so the
    /// AI is told what they are -- or it reads a voice it has never
    /// heard of as one of the people.
    #[test]
    fn a_bot_is_told_which_of_the_names_are_the_other_ais() {
        let (agent, t) = turn_among(Cue::Speak, &["Ryan"], false, &["Cora"]);
        let system = system_of(&agent, &t);
        assert!(system.contains("Cora is in this room with you"), "{system}");
        assert!(system.contains("an AI voice like you, not a person"));
        assert!(
            system.contains("don't speak for Cora"),
            "by name, never a pronoun"
        );
        // And it is talking *with* the people, who do not include Cora.
        assert!(system.contains("voice chat with Ryan."), "{system}");

        // Two of them, and the note is in the plural.
        let (agent, t) = turn_among(Cue::Speak, &["Ryan"], false, &["Cora", "Ada"]);
        let two = system_of(&agent, &t);
        assert!(
            two.contains("Cora and Ada are in this room with you"),
            "{two}"
        );
        assert!(two.contains("AI voices like you, not people"));

        // Alone with the people, there is no such note at all.
        let (agent, t) = turn_with(Cue::Speak, &["Ryan"], false);
        let alone = system_of(&agent, &t);
        assert!(!alone.contains("in this room with you"));

        // Judging a pause, it is told a pause may be the other one's.
        let (agent, mut t) = turn_among(judge(None, false), &["Ryan"], false, &["Cora"]);
        let note = agent.messages(&t, None).pop().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(note.contains("Ryan is the only person here."), "{note}");
        assert!(note.contains("Cora is in the room too"));
        assert!(
            !note.contains("nobody else they could be talking to"),
            "which is no longer true"
        );

        // Unless that one has been silenced: then there is nothing to
        // leave to it, and the pause is this bot's or nobody's. It is
        // still one of the voices in the room, though -- its name is in
        // the transcript and must not read as a person's.
        t.others_answering.clear();
        let system = system_of(&agent, &t);
        assert!(system.contains("Cora is in this room with you"), "{system}");
        let note = agent.messages(&t, None).pop().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            note.contains("nobody else they could be talking to"),
            "{note}"
        );
        assert!(!note.contains("Cora is in the room too"));
    }

    /// A turn another voice put to it, and the one after which the
    /// people have to come back in. The second is told, never
    /// discovered: a bot that finds out by calling `hand_to` and being
    /// refused has already written a reply for a conversation that is
    /// about to stop dead.
    #[test]
    fn a_handed_turn_says_who_handed_it_and_when_it_is_the_last() {
        let cue = Cue::Handed {
            from: "Cora".into(),
        };
        let (agent, mut t) = turn_among(cue, &["Ryan"], false, &["Cora"]);
        let note = |t: &Turn| {
            agent.messages(t, None).pop().unwrap()["content"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let handed = note(&t);
        assert!(handed.contains("Cora has put something to you"), "{handed}");
        assert!(handed.contains("stay_quiet"), "it may still pass");

        // With one of the two left, that is all it is told.
        assert_eq!(t.hand_on, 2);
        assert!(!handed.contains("last thing the voices here can say"));

        // With none left, the last word is the people's, and it is
        // asked to leave it with them.
        t.hand_on = 0;
        let last = note(&t);
        assert!(
            last.contains("last thing the voices here can say"),
            "{last}"
        );
        assert!(last.contains("leave it with the people"));

        // Alone in the room there is no such thing to say at all.
        let (agent, mut t) = turn_with(Cue::Speak, &["Ryan"], false);
        t.hand_on = 0;
        let alone = agent.messages(&t, None);
        assert_eq!(alone.len(), 2, "system and the conversation: {alone:?}");
    }

    #[test]
    fn look_up_is_offered_only_when_search_is_on() {
        let without = tool_names(true, false);
        assert!(without.contains(&"save_game".to_string()));
        assert!(!without.contains(&"look_up".to_string()));

        let with = tool_names(true, true);
        // The game and card tools are untouched; look_up is added to them.
        assert_eq!(with.len(), without.len() + 1);
        assert_eq!(&with[..without.len()], &without[..]);
        assert_eq!(with.last().unwrap(), "look_up");
    }

    /// The endpoint the environment points at, for the live tests.
    fn configured_chat() -> Chat {
        let Ok(url) = std::env::var("VOX_SCRIBE_LLM_URL") else {
            panic!("set VOX_SCRIBE_LLM_URL, VOX_SCRIBE_LLM_MODEL and VOX_SCRIBE_LLM_KEY");
        };
        Chat {
            url,
            model: std::env::var("VOX_SCRIBE_LLM_MODEL").expect("VOX_SCRIBE_LLM_MODEL"),
            key: std::env::var("VOX_SCRIBE_LLM_KEY").ok(),
            reasoning: std::env::var("VOX_ROOM_REASONING")
                .ok()
                .and_then(|r| Reasoning::parse(&r))
                .unwrap_or(Reasoning::Low),
            api: std::env::var("VOX_ROOM_LLM_API")
                .ok()
                .and_then(|a| Api::parse(&a))
                .unwrap_or_default(),
            search: false,
        }
    }

    fn turn(cue: Cue) -> (Agent, Turn) {
        turn_with(cue, &["Ryan", "Alex"], false)
    }

    /// A pause the AI is to judge for itself: `lull` is a silence it
    /// asked to be shown again, `over` that the words came in while it
    /// was still speaking.
    fn judge(lull: Option<u64>, over: bool) -> Cue {
        Cue::Decide { lull, over }
    }

    /// An agent and a turn for it. The room's own settings are not in
    /// it any more: what the AI is told about itself comes from the
    /// agent, because in a room with two of them the room could only
    /// have said one of their names.
    fn turn_with(cue: Cue, people: &[&str], engaged: bool) -> (Agent, Turn) {
        let (agent, turn) = turn_among(cue, people, engaged, &[]);
        (agent, turn)
    }

    /// The same, in a room with `others` -- the other AIs in it.
    fn turn_among(cue: Cue, people: &[&str], engaged: bool, others: &[&str]) -> (Agent, Turn) {
        let agent = Agent {
            llm: Llm::Echo,
            name: "Vox".into(),
            prompt: DEFAULT_PROMPT.to_string(),
            allowed: Allowed::Everything,
            generation: Arc::new(AtomicU64::new(0)),
            translate: None,
            art: Arc::new(Art::default()),
            clones: None,
            presets: Vec::new(),
        };
        let turn = Turn {
            generation: 1,
            messages: vec![(Role::User, "Ryan: so what did you make of it?".into())],
            asked: Instant::now(),
            game: None,
            people: people.iter().map(|p| p.to_string()).collect(),
            others: others.iter().map(|o| o.to_string()).collect(),
            others_answering: others.iter().map(|o| o.to_string()).collect(),
            hand_on: if others.is_empty() { 0 } else { 2 },
            puppets: Vec::new(),
            showing: None,
            playhouse: false,
            in_box: Vec::new(),
            on_stage: Vec::new(),
            language: "English".into(),
            speaks: Vec::new(),
            engaged,
            cue,
        };
        (agent, turn)
    }

    /// A room to make pictures in.
    fn picture_room() -> Arc<Room> {
        let store = crate::store::Store::memory();
        store.invite("Ryan").unwrap();
        let (room, _wiring) = Room::new(
            store,
            RoomConfig {
                id: "Lobby".into(),
                title: "Lobby".into(),
                only: None,
                default_mode: Mode::Named,
                bots: vec![BotConfig::new("Vox", vec![])],
                reply_after: Duration::from_millis(1200),
                confirm: Duration::from_millis(300),
                engaged_for: Duration::from_secs(30),
                history: 60,
                ai_turns: 2,
                language: "English".into(),
                languages: vec!["English".into()],
                spoken: vec!["English".into()],
            },
            None,
            std::collections::BTreeMap::new(),
            None,
            None,
            None,
        )
        .unwrap();
        room
    }

    /// An MCP server that answers the handshake and then draws one
    /// 8x8 PNG, framed as SSE the way a real one does.
    ///
    /// Enough of a server to put the room's whole half of the flow
    /// through its paces without a model, a key or any money: the
    /// thing that was never covered until a picture came out as the
    /// bare number 1 on somebody's screen.
    fn mock_mcp(draws: bool) -> String {
        use std::io::{BufRead as _, BufReader, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = n.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; len];
                std::io::Read::read_exact(&mut reader, &mut body).unwrap_or(());
                let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let method = req["method"].as_str().unwrap_or("");
                // A notification wants no answer but the 202.
                if req["id"].is_null() {
                    let mut s = stream;
                    let _ = s.write_all(
                        b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\
                          Connection: close\r\n\r\n",
                    );
                    continue;
                }
                let result = match method {
                    "initialize" => json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "serverInfo": { "name": "mock", "version": "1" },
                    }),
                    // The two shapes a picture server comes in: one
                    // that draws from a description, and one that
                    // changes a picture it already drew -- which is
                    // asked in two parts, and whose words are under
                    // `instruction` rather than `prompt`. The key as
                    // an argument is on the first of them because
                    // some servers do want it there, and that path
                    // has to keep working too.
                    //
                    // The one that draws has a `style`, the way the
                    // house's own server does: the looks it can draw
                    // in, as an enum the model picks out of. The room
                    // decides nothing about it and hands it straight
                    // on; see the test that a style is passed through
                    // untouched. The one that changes a picture has
                    // none, because a change keeps the look it was
                    // drawn in.
                    "tools/list" => json!({ "tools": [{
                        "name": "generate_pixel_art",
                        "description": "Draw something.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "prompt": { "type": "string" },
                                "style": {
                                    "type": "string",
                                    "enum": ["pixel-art", "cel-shaded"],
                                },
                                "rd_api_key": { "type": "string" },
                            },
                            "required": ["prompt"],
                        },
                    }, {
                        "name": "refine_pixel_art",
                        "description": "Change one already drawn.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "job_id": { "type": "string" },
                                "instruction": { "type": "string" },
                            },
                            "required": ["job_id", "instruction"],
                        },
                    }, {
                        "name": "list_jobs",
                        "description": "Say what was drawn before.",
                        "inputSchema": { "type": "object", "properties": {} },
                    }, {
                        "name": "list_styles",
                        "description": "Say what looks it can draw in.",
                        "inputSchema": { "type": "object", "properties": {} },
                    }] }),
                    "tools/call"
                        if matches!(
                            req["params"]["name"].as_str(),
                            Some("generate_pixel_art" | "refine_pixel_art")
                        ) && draws =>
                    {
                        use base64::Engine as _;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(a_png());
                        // A handle in the words, the way a real
                        // picture server answers: the id you pass
                        // back to change this one. And the style it
                        // drew in when the call named one, which is
                        // also what a real one says.
                        let said = match req["params"]["arguments"]["style"].as_str() {
                            Some(style) => format!("Job mock7f3: drawn, {style}."),
                            None => "Job mock7f3: drawn.".to_string(),
                        };
                        json!({ "content": [
                            { "type": "text", "text": said },
                            { "type": "image", "data": b64, "mimeType": "image/png" },
                        ] })
                    }
                    // A call that draws nothing: the list of what
                    // came before, or a generation that came back
                    // empty.
                    "tools/call" => json!({
                        "content": [{ "type": "text", "text": "nothing to draw" }]
                    }),
                    _ => json!({}),
                };
                let answer = json!({ "jsonrpc": "2.0", "id": req["id"], "result": result });
                // SSE, because that is what a real one sends.
                let payload = format!("event: message\ndata: {answer}\n\n");
                // One request per connection, so every answer says
                // so: left to keep-alive, the client reuses a socket
                // this loop has already dropped.
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Connection: close\r\nContent-Length: {}\r\n\r\n",
                    payload.len()
                );
                let mut s = stream;
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(payload.as_bytes());
            }
        });
        url
    }

    fn a_png() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        v.extend_from_slice(b"the rest of it");
        v
    }

    /// The room's whole half of making a picture, against a server
    /// that behaves like a real one: a card goes up with a spinner,
    /// the call is made, the picture fills the card, and the model is
    /// told the number and nothing else.
    #[test]
    fn a_picture_goes_up_before_the_call_and_fills_in_after() {
        let room = picture_room();
        let g = room.generation_of_only_bot().load(Ordering::SeqCst);
        let server = Arc::new(mcp::Server::new(mcp::Config {
            name: "art".into(),
            url: mock_mcp(true),
            key: "secret".into(),
            only: vec!["generate_pixel_art".into(), "list_jobs".into()],
        }));
        assert_eq!(server.connect().expect("hello"), 2);

        let args = json!({ "prompt": "a stern Borg commander", "size": 128 });
        let said = drawing(&room, g, &server, "art_generate_pixel_art", &args)
            .expect("the call went through");

        // The model is told the number and told to wait for a yes.
        let said = said["result"].as_str().unwrap_or_default().to_string();
        assert!(said.contains("Picture 1"), "{said}");
        assert!(said.contains("set_avatar"), "{said}");
        assert!(!said.contains("PNG"), "and not the picture itself: {said}");

        // The card is in the conversation, finished, and showing what
        // it was asked for.
        let entry = room
            .log_now()
            .into_iter()
            .find(|e| e.kind == crate::room::Kind::Picture)
            .expect("a picture card is in the conversation");
        assert_eq!(
            entry.text, "picture 1",
            "what the model was told to call it"
        );
        assert_eq!(entry.title, "a stern Borg commander");
        assert!(!entry.live, "and it is not still spinning");
        assert_eq!(room.picture_png(&entry.id).unwrap().unwrap(), a_png());
        // And what the tool said is kept on the card, because the
        // handle for changing this picture is in it and the request
        // to change it always comes in a later turn, by which time
        // the tool's reply is gone. See `room::Entry::note`.
        assert_eq!(entry.note, "Job mock7f3: drawn.");

        // Which is a face somebody can now be given.
        assert_eq!(room.set_avatar(g, "Ryan", 1).unwrap(), "Ryan");
        assert_eq!(room.avatar_png("Ryan", None).unwrap().unwrap().0, a_png());
    }

    /// Changing a picture is making one, as far as the room is
    /// concerned: a card goes up for it too. The tool that does it is
    /// asked in two parts and only one of them is words, under a name
    /// the room has to know to look under -- a room that only looked
    /// for `prompt` would put no card up and caption nothing.
    #[test]
    fn a_change_to_a_picture_gets_a_card_of_its_own() {
        let room = picture_room();
        let g = room.generation_of_only_bot().load(Ordering::SeqCst);
        let server = Arc::new(mcp::Server::new(mcp::Config {
            name: "art".into(),
            url: mock_mcp(true),
            key: String::new(),
            only: vec!["refine_pixel_art".into()],
        }));
        assert_eq!(server.connect().expect("hello"), 1);

        let args = json!({ "job_id": "41fc", "instruction": "give it a red top hat" });
        let said = drawing(&room, g, &server, "art_refine_pixel_art", &args)
            .expect("the call went through");
        assert!(said["result"]
            .as_str()
            .unwrap_or_default()
            .contains("Picture 1"));

        let entry = room
            .log_now()
            .into_iter()
            .find(|e| e.kind == crate::room::Kind::Picture)
            .expect("a picture card is in the conversation");
        assert_eq!(
            entry.title, "give it a red top hat",
            "captioned with what was asked for, which for a change is the change"
        );
        assert!(!entry.live, "and it is not still spinning");
        assert_eq!(room.picture_png(&entry.id).unwrap().unwrap(), a_png());
        assert_eq!(
            entry.note, "Job mock7f3: drawn.",
            "a refine's own handle is kept too, so it can be refined again"
        );
    }

    /// A look the model picked reaches the server as it picked it, and
    /// both looks are in front of it to pick from.
    ///
    /// The room decides nothing about how a picture is drawn -- what
    /// looks there are is the server's to publish and the person in
    /// the room's to choose between, and the room's whole part is to
    /// not get in the way of either. So this checks the two halves of
    /// not getting in the way: that the enum the server published is
    /// still in the schema the model is shown, next to the key that
    /// was taken out of it, and that the style named in the call is
    /// what the server was asked for.
    #[test]
    fn a_style_is_passed_through_untouched() {
        let room = picture_room();
        let g = room.generation_of_only_bot().load(Ordering::SeqCst);
        let server = Arc::new(mcp::Server::new(mcp::Config {
            name: "art".into(),
            url: mock_mcp(true),
            key: "secret".into(),
            only: vec!["generate_pixel_art".into(), "list_styles".into()],
        }));
        assert_eq!(server.connect().expect("hello"), 2);

        // What the model is shown: the looks it may ask for, and no
        // key to invent.
        let offered = server.offer();
        let draws = offered
            .iter()
            .find(|t| t["function"]["name"] == "art_generate_pixel_art")
            .expect("the tool that draws is lent");
        let props = &draws["function"]["parameters"]["properties"];
        assert_eq!(
            props["style"]["enum"].as_array().unwrap(),
            &[json!("pixel-art"), json!("cel-shaded")],
            "both looks are in front of it: {props}"
        );
        assert!(
            props["rd_api_key"].is_null(),
            "and the key still is not: {props}"
        );

        let args = json!({ "prompt": "a dwarf blacksmith", "style": "cel-shaded" });
        let said = drawing(&room, g, &server, "art_generate_pixel_art", &args)
            .expect("the call went through");
        assert!(said["result"]
            .as_str()
            .unwrap_or_default()
            .contains("Picture 1"));

        let entry = room
            .log_now()
            .into_iter()
            .find(|e| e.kind == crate::room::Kind::Picture)
            .expect("a picture card is in the conversation");
        assert_eq!(
            entry.title, "a dwarf blacksmith",
            "captioned with the words and not with the look"
        );
        assert_eq!(
            entry.note, "Job mock7f3: drawn, cel-shaded.",
            "the server drew in the look that was asked for"
        );
        assert!(!entry.live);
        // And it is a face, the same as a sprite is: nothing about
        // wearing one knows which look it was drawn in.
        assert_eq!(room.set_avatar(g, "Ryan", 1).unwrap(), "Ryan");
    }

    /// A call that draws nothing leaves no card: a list of old work is
    /// not a portrait, and the room must not spin on one.
    #[test]
    fn a_call_that_draws_nothing_puts_no_card_up() {
        let room = picture_room();
        let g = room.generation_of_only_bot().load(Ordering::SeqCst);
        let server = Arc::new(mcp::Server::new(mcp::Config {
            name: "art".into(),
            url: mock_mcp(false),
            key: String::new(),
            only: vec![],
        }));
        server.connect().expect("hello");

        // No words anywhere in the call, so nothing was ever going
        // to be drawn. Asking what looks there are is the same: it is
        // a question, asked in the middle of an interview, and a
        // spinner for it would promise a picture nobody is making.
        for (tool, args) in [
            ("art_list_jobs", json!({ "limit": 10 })),
            ("art_list_styles", json!({})),
        ] {
            let said = drawing(&room, g, &server, tool, &args).unwrap();
            assert_eq!(said["result"], "nothing to draw", "{tool}");
            assert!(
                !room
                    .log_now()
                    .iter()
                    .any(|e| e.kind == crate::room::Kind::Picture),
                "no card for {tool}, which was never going to draw"
            );
        }

        // A prompt that came back with nothing does leave a card --
        // the room had already promised one -- and it says so rather
        // than spinning for ever.
        let said = drawing(
            &room,
            g,
            &server,
            "art_generate_pixel_art",
            &json!({ "prompt": "a hat" }),
        )
        .unwrap();
        assert_eq!(said["result"], "nothing to draw");
        let entry = room
            .log_now()
            .into_iter()
            .find(|e| e.kind == crate::room::Kind::Picture)
            .expect("the card that was promised");
        assert!(!entry.live, "and it stopped waiting");
        assert!(room.picture_png(&entry.id).unwrap().is_none());
    }

    /// Which tool answers have to go back to the model even when it has
    /// already spoken. Everything `run_tool` refuses is an `error` and
    /// nothing it allows is, so this is the whole of the test for
    /// "that promise it just made is not going to be kept".
    #[test]
    fn a_tool_that_failed_is_told_apart_from_one_that_worked() {
        for worked in [
            json!({ "ok": true }),
            json!({ "people": [] }),
            json!({ "sample": 1, "who": "Ryan" }),
            // A tool that reported somebody else's error as its result
            // is still a tool that worked.
            json!({ "result": { "error": "upstream said no" } }),
        ] {
            assert!(!went_wrong(&worked.to_string()), "{worked}");
        }
        for failed in [
            json!({ "error": "Ryan's microphone is muted" }),
            json!({ "error": "you have no tool called \"speak_as\"" }),
        ] {
            assert!(went_wrong(&failed.to_string()), "{failed}");
        }
        // Not JSON at all is not a failure the model needs telling
        // about; it is the room having a bug, and guessing otherwise
        // would send every reply round again.
        assert!(!went_wrong("not json"));
    }

    /// A whole reply, through the real thing: the room's own request,
    /// the stream parsed as it arrives, a tool run from it, and the
    /// model carrying on from the tool's answer. Not run by default:
    /// it talks to the configured endpoint.
    ///
    /// ```text
    /// cargo test -p vox_room -- --ignored --nocapture whole_reply
    /// ```
    #[test]
    #[ignore = "asks the configured model for a whole reply"]
    fn a_whole_reply_goes_out_and_comes_back() {
        let chat = configured_chat();
        println!("speaking {:?} to {}", chat.api(), chat.url);
        let store = crate::store::Store::memory();
        store.invite("Ryan").unwrap();
        let (room, _wiring) = Room::new(
            store,
            RoomConfig {
                id: "Lobby".into(),
                title: "Lobby".into(),
                only: None,
                default_mode: Mode::Named,
                bots: vec![BotConfig::new("Vox", vec![])],
                reply_after: Duration::from_millis(1200),
                confirm: Duration::from_millis(300),
                engaged_for: Duration::from_secs(30),
                history: 60,
                ai_turns: 2,
                language: "English".into(),
                languages: vec!["English".into()],
                spoken: vec!["English".into()],
            },
            None,
            std::collections::BTreeMap::new(),
            None,
            None,
            None,
        )
        .unwrap();

        let (agent, mut t) = turn_with(Cue::Speak, &["Ryan"], false);
        t.messages = vec![(
            Role::User,
            "Ryan: Vox, put the rules up for everyone: one card each, aces low. \
             Use post_card, then tell me in one short sentence that it is up."
                .into(),
        )];
        let (sender, rx) = crossbeam_channel::unbounded();
        let tx = Voices(vec![sender]);
        let http = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(120))
            .build();
        stream_reply(
            &http,
            &room,
            &chat,
            &agent.messages(&t, None),
            Allowed::Everything,
            true,
            t.cue.clone(),
            t.hand_on > 0,
            7,
            Instant::now(),
            &room.generation_of_only_bot(),
            &tx,
            &Art::default(),
            None,
            None,
        )
        .expect("the reply");
        drop(tx);

        let said: Vec<String> = rx
            .iter()
            .filter_map(|s| match s {
                Speak::Sentence(_, s) => Some(s),
                _ => None,
            })
            .collect();
        let posted = room.read_cards(None); // not these: the card went to the room
        let log = room.hello("Ryan")["log"].to_string();
        println!("  said: {said:?}");
        println!("  posted: {}", &log[..log.len().min(300)]);
        assert!(posted
            .iter()
            .all(|p| p["cards"].as_array().unwrap().is_empty()));
        assert!(log.contains("\"kind\":\"card\""), "the tool ran: {log}");
        assert!(
            log.contains("aces") || log.contains("Aces"),
            "with its body: {log}"
        );
        assert!(
            !said.is_empty(),
            "it carried on talking after the tool answered"
        );
    }

    /// Put a decide turn to the configured model and see what it does
    /// with it. Not run by default: it talks to the endpoint in
    /// VOX_SCRIBE_LLM_URL and costs whatever that costs.
    ///
    /// ```text
    /// cargo test -p vox_room -- --ignored --nocapture judges
    /// ```
    #[test]
    #[ignore = "asks the configured model to judge some pauses"]
    fn the_model_judges_a_pause_the_way_the_room_needs() {
        let chat = configured_chat();
        // (said, who is here, mid-conversation, should it take the turn)
        let cases: &[(&str, &[&str], bool, bool)] = &[
            ("Ryan: So what do you make of that?", &["Ryan"], false, true),
            (
                "Ryan: Right, and how long would that take?",
                &["Ryan"],
                true,
                true,
            ),
            (
                "Ryan: Did you get the tickets?\nAlex: Yeah, two for Saturday.",
                &["Ryan", "Alex"],
                false,
                false,
            ),
            (
                "Ryan: I'm going to make some tea.\nAlex: Grab me one.",
                &["Ryan", "Alex"],
                false,
                false,
            ),
        ];
        let mut wrong = Vec::new();
        let mut queue: Vec<_> = cases.iter().rev().collect();
        while let Some(case) = queue.pop() {
            let (said, people, engaged, want_turn) = case;
            let (agent, mut t) = turn_with(judge(None, false), people, *engaged);
            t.messages = vec![(Role::User, said.to_string())];
            // Ask the way the room asks, on whichever API it speaks.
            let offered = tools_for(
                Allowed::Everything,
                true,
                false,
                true,
                true,
                &Art::default(),
                false,
                None,
            );
            let messages = agent.messages(&t, None);
            let (path, mut body) = match chat.api() {
                Api::Responses => (
                    "responses",
                    json!({
                        "model": chat.model,
                        "input": messages,
                        "tools": flatten_tools(&offered),
                        "reasoning": { "effort": chat.reasoning.as_str() },
                    }),
                ),
                _ => (
                    "chat/completions",
                    json!({
                        "model": chat.model,
                        "messages": messages,
                        "tools": offered,
                    }),
                ),
            };
            if chat.api() != Api::Responses && REASONING_OFF.load(Ordering::Relaxed) {
                body["reasoning_effort"] = json!("none");
            }
            let resp: Value = match chat
                .authorize(ureq::post(&chat.endpoint(path)))
                .send_json(body)
            {
                Ok(r) => r.into_json().expect("read the answer"),
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    // The same lesson the room learns on its first call.
                    if code == 400
                        && detail.contains("reasoning_effort")
                        && !REASONING_OFF.swap(true, Ordering::Relaxed)
                    {
                        println!("(this endpoint only takes tools with reasoning off)");
                        queue.push(case);
                        continue;
                    }
                    panic!("HTTP {code}: {detail}")
                }
                Err(e) => panic!("{e}"),
            };
            // Either API's answer: did it call stay_quiet, or talk?
            let (quiet, spoke) = match chat.api() {
                Api::Responses => {
                    let out = resp["output"].as_array().cloned().unwrap_or_default();
                    let said: String = out
                        .iter()
                        .filter(|i| i["type"] == "message")
                        .flat_map(|i| i["content"].as_array().cloned().unwrap_or_default())
                        .filter_map(|c| c["text"].as_str().map(str::to_string))
                        .collect();
                    (
                        out.iter()
                            .any(|i| i["type"] == "function_call" && i["name"] == "stay_quiet"),
                        said,
                    )
                }
                _ => {
                    let m = &resp["choices"][0]["message"];
                    (
                        m["tool_calls"].as_array().is_some_and(|c| {
                            c.iter().any(|c| c["function"]["name"] == "stay_quiet")
                        }),
                        m["content"].as_str().unwrap_or_default().to_string(),
                    )
                }
            };
            let took_turn = !quiet;
            println!(
                "{} {said:?}\n   -> {}",
                if took_turn == *want_turn {
                    "ok  "
                } else {
                    "WRONG"
                },
                if quiet {
                    "stayed quiet".to_string()
                } else {
                    format!("spoke: {}", spoke.trim())
                },
            );
            if took_turn != *want_turn {
                wrong.push(*said);
            }
        }
        assert!(wrong.is_empty(), "judged wrongly: {wrong:#?}");
    }

    #[test]
    fn each_api_is_fed_and_read_in_its_own_shape() {
        // One function tool, wrapped for chat and flat for responses.
        let offered = tools_for(
            Allowed::Everything,
            false,
            false,
            false,
            true,
            &Art::default(),
            false,
            None,
        );
        let flat = flatten_tools(&offered);
        assert_eq!(offered[0]["function"]["name"], "list_people");
        assert_eq!(flat[0]["name"], "list_people");
        assert_eq!(flat[0]["type"], "function");
        assert!(flat[0]["function"].is_null(), "not wrapped twice");
        assert_eq!(flat[0]["parameters"], offered[0]["function"]["parameters"]);

        // Reading a streamed answer: text, then a call, then the end.
        let fold = |api, events: &[Value]| {
            let (mut said, mut pending, mut calls) = (String::new(), String::new(), Vec::new());
            let over = events
                .iter()
                .map(|e| fold_event(api, e, &mut said, &mut pending, &mut calls))
                .collect::<Vec<_>>()
                .contains(&true);
            (said, calls, over)
        };
        let (said, calls, over) = fold(
            Api::Responses,
            &[
                json!({ "type": "response.output_text.delta", "delta": "Dealing " }),
                json!({ "type": "response.output_text.delta", "delta": "you in." }),
                json!({ "type": "response.reasoning_summary_text.delta", "delta": "never spoken" }),
                json!({
                    "type": "response.output_item.done",
                    "output_index": 1,
                    "item": {
                        "type": "function_call",
                        "call_id": "call_2",
                        "name": "send_card",
                        "arguments": "{\"to\":\"Alex\"}",
                    },
                }),
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "post_card",
                        "arguments": "{\"title\":\"Rules\"}",
                    },
                }),
                json!({ "type": "response.completed" }),
            ],
        );
        assert_eq!(said, "Dealing you in.", "the thinking is not the answer");
        assert!(over, "the stream says when it is done");
        // Each call lands at its own output_index, in order.
        assert_eq!(
            calls
                .iter()
                .map(|c| (c.name.as_str(), c.id.as_str()))
                .collect::<Vec<_>>(),
            [("post_card", "call_1"), ("send_card", "call_2")]
        );

        // The same, assembled from chat's deltas.
        let (said, calls, over) = fold(
            Api::Chat,
            &[
                json!({ "choices": [{ "delta": { "content": "Dealing " } }] }),
                json!({ "choices": [{ "delta": { "content": "you in." } }] }),
                json!({ "choices": [{ "delta": { "tool_calls": [{
                    "index": 0, "id": "call_1",
                    "function": { "name": "post_card", "arguments": "{\"title\":" },
                }] } }] }),
                json!({ "choices": [{ "delta": { "tool_calls": [{
                    "index": 0, "function": { "arguments": "\"Rules\"}" },
                }] } }] }),
            ],
        );
        assert_eq!(said, "Dealing you in.");
        assert!(!over, "chat ends on [DONE], not an event");
        assert_eq!(calls[0].arguments, r#"{"title":"Rules"}"#);

        // And handing the results back, each API's way.
        let call = ToolCall {
            id: "call_1".into(),
            name: "post_card".into(),
            arguments: "{}".into(),
        };
        let mut convo = Vec::new();
        push_turn(
            Api::Responses,
            &mut convo,
            "Here you go.",
            std::slice::from_ref(&call),
        );
        push_result(
            Api::Responses,
            &mut convo,
            &call,
            0,
            "{\"posted\":true}".into(),
        );
        assert_eq!(
            convo[0],
            json!({ "role": "assistant", "content": "Here you go." })
        );
        assert_eq!(convo[1]["type"], "function_call");
        assert_eq!(convo[1]["call_id"], "call_1");
        assert_eq!(
            convo[2],
            json!({ "type": "function_call_output", "call_id": "call_1", "output": "{\"posted\":true}" })
        );

        let mut convo = Vec::new();
        push_turn(
            Api::Chat,
            &mut convo,
            "Here you go.",
            std::slice::from_ref(&call),
        );
        push_result(Api::Chat, &mut convo, &call, 0, "{}".into());
        assert_eq!(convo[0]["role"], "assistant");
        assert_eq!(convo[0]["tool_calls"][0]["function"]["name"], "post_card");
        assert_eq!(convo[1]["role"], "tool");
        assert_eq!(convo[1]["tool_call_id"], "call_1");
    }

    #[test]
    fn the_endpoint_is_asked_for_as_configured() {
        let chat = |api| Chat {
            url: "http://x/v1".into(),
            model: "m".into(),
            key: None,
            reasoning: Reasoning::Medium,
            api,
            search: false,
        };
        assert_eq!(chat(Api::Chat).api(), Api::Chat);
        assert_eq!(chat(Api::Responses).api(), Api::Responses);
        // Auto reaches for /responses until an endpoint hasn't got one.
        NO_RESPONSES.store(false, Ordering::Relaxed);
        assert_eq!(chat(Api::Auto).api(), Api::Responses);
        NO_RESPONSES.store(true, Ordering::Relaxed);
        assert_eq!(chat(Api::Auto).api(), Api::Chat);
        assert_eq!(
            chat(Api::Responses).api(),
            Api::Responses,
            "asked for, so used"
        );
        NO_RESPONSES.store(false, Ordering::Relaxed);

        assert_eq!(Reasoning::parse("HIGH"), Some(Reasoning::High));
        assert_eq!(Reasoning::parse("off"), Some(Reasoning::None));
        assert_eq!(Reasoning::parse("sure"), None);
        assert!(!Reasoning::None.thinks() && Reasoning::Low.thinks());
        assert_eq!(Api::parse("responses"), Some(Api::Responses));
        assert_eq!(Api::parse("nope"), None);
    }

    #[test]
    fn a_turn_to_judge_says_who_it_is_being_judged_among() {
        let note = |people: &[&str], engaged| {
            let (agent, t) = turn_with(judge(None, false), people, engaged);
            agent.messages(&t, None).pop().unwrap()["content"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // One person and nobody else: the room cannot be talking to
        // anyone but the AI, which is what it was getting wrong.
        let alone = note(&["Ryan"], false);
        assert!(alone.contains("Ryan is the only person here"), "{alone}");
        assert!(alone.contains("nobody else they could be talking to"));
        assert!(!alone.contains("some of what is said is between them"));

        // Several, and a pause may well be theirs.
        let crowd = note(&["Ryan", "Alex", "Sam"], false);
        assert!(crowd.contains("Ryan, Alex and Sam are here"), "{crowd}");
        assert!(crowd.contains("between them"));

        // Mid-conversation, the turn is the AI's unless they turn away.
        assert!(!alone.contains("in the middle of a conversation"));
        let mid = note(&["Ryan", "Alex"], true);
        assert!(mid.contains("in the middle of a conversation"), "{mid}");
        assert!(mid.contains("carried on without naming you"));
        assert!(!mid.contains("{engaged}") && !mid.contains("{here}"));
    }

    #[test]
    fn a_turn_to_judge_ends_with_what_makes_it_a_judgment() {
        let (agent, t) = turn(judge(None, false));
        let messages = agent.messages(&t, None);
        let last = messages.last().unwrap();
        assert_eq!(last["role"], "user");
        let note = last["content"].as_str().unwrap();
        assert!(note.contains("stay_quiet"), "{note}");
        assert!(note.contains("Nobody has said your name"));
        // The conversation itself is untouched, and the note is last, so
        // the rest of the prompt is the same either way.
        assert_eq!(messages[1]["content"], "Ryan: so what did you make of it?");

        // Talked over and left to finish: it is told so, because the
        // likeliest thing in that pause is a cough.
        let (agent, t) = turn(judge(None, true));
        let over = agent.messages(&t, None).pop().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(over.contains("while you were still speaking"), "{over}");
        assert!(over.contains("Take the turn only if it was plainly for you"));
        assert!(!over.contains("{over}") && !note.contains("while you were still speaking"));

        // The silence it asked for says how long it has lasted.
        let (agent, t) = turn(judge(Some(8), false));
        let note = agent.messages(&t, None);
        let note = note.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(note.contains("quiet for 8 seconds"), "{note}");
        assert!(!note.contains("{lull}"));

        // A turn it was told to take carries no such note at all.
        let (agent, t) = turn(Cue::Speak);
        let messages = agent.messages(&t, None);
        assert_eq!(messages.len(), 2, "system and the conversation");
        assert!(!messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("stay_quiet"));
    }

    #[test]
    fn the_cards_are_called_cards_where_the_room_can_hear() {
        // Everything the AI is told to do is said in a voice the whole
        // room hears, and "check your private area" is not a thing to
        // say to a room. The prompt hands it the words to use instead.
        assert!(CARDS_NOTE.contains("check your cards"));
        assert!(CARDS_NOTE.contains("never tell anyone to check their private area"));

        // The tools are read as instructions for what to do, so nothing
        // in them is phrased that way at all.
        for (lobby, deciding) in [(true, true), (false, false)] {
            let tools = tools_for(
                Allowed::Everything,
                lobby,
                true,
                deciding,
                true,
                &Art::default(),
                false,
                None,
            )
            .to_string();
            assert!(!tools.contains("private"), "{tools}");
        }
    }

    #[test]
    fn staying_quiet_is_offered_only_when_the_room_leaves_it_to_the_ai() {
        for lobby in [true, false] {
            assert!(
                !tool_names(lobby, true).contains(&"stay_quiet".to_string()),
                "a turn it was told to take has no way to decline it"
            );
            let deciding = tool_names_deciding(lobby, true, true);
            assert!(deciding.contains(&"stay_quiet".to_string()));
            // Everything else it had, it still has: it may well speak.
            for t in tool_names(lobby, true) {
                assert!(deciding.contains(&t), "{t} went missing");
            }
        }
    }

    #[test]
    fn the_room_tools_are_offered_in_a_game_and_the_game_tools_are_not() {
        let in_game = tool_names(false, true);
        assert_eq!(
            in_game,
            [
                "list_people",
                "send_card",
                "post_card",
                "take_back_card",
                "read_cards",
                "hand_to",
                "wait_until_named",
                // With no picture server anywhere -- `Art::default()`
                // -- and the faces still here: a picture can be
                // dropped into any room by whoever is in it, so there
                // is one to give and lines to write about it. What a
                // server adds is the drawing.
                "set_avatar",
                "set_bio",
            ]
        );
        // In the lobby they are there as well, alongside the games.
        let lobby = tool_names(true, false);
        for t in &in_game {
            assert!(lobby.contains(t), "{t} is missing from the lobby");
        }
        assert!(lobby.contains(&"save_game".to_string()));
    }

    #[test]
    fn a_game_is_told_about_the_cards_without_being_told_about_games() {
        let frame = game_prompt("Dragon Heist", "Run a heist.", Allowed::Everything);
        assert!(frame.contains("send_card"));
        assert!(frame.contains("list_people"));
        assert!(frame.contains("wait_until_named"));
        assert!(!frame.contains("{cards}"));
        assert!(!frame.contains("save_game"));
    }

    /// The shape a real /responses search comes back in (field names and
    /// item order captured from the live endpoint).
    #[test]
    fn a_search_answer_is_the_message_and_never_the_reasoning() {
        let body = json!({
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [],
                    // Empty from the live endpoint, but not by contract:
                    // if it ever fills, it must not be spoken.
                    "content": [{ "type": "reasoning_text", "text": "they probably mean the city" }],
                },
                { "type": "web_search_call", "status": "completed" },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "Gaia Masala and Burger is well liked in Boulder.",
                        "annotations": [{ "type": "url_citation", "url": "https://example.com" }],
                    }],
                },
            ],
        });
        assert_eq!(
            search_text(&body),
            "Gaia Masala and Burger is well liked in Boulder."
        );
        // Nothing to say is an error upstream, not an empty reply.
        assert_eq!(search_text(&json!({ "output": [] })), "");
        assert_eq!(search_text(&json!({ "error": { "message": "nope" } })), "");
    }

    #[test]
    fn cleans_markdown() {
        assert_eq!(clean("  **Sure**,  here's   `one`. "), "Sure, here's one.");
    }

    // -- puppets ---------------------------------------------------------

    /// The stage, as the agent thread would have it: the room, and the
    /// voices the operator said the speech server has.
    fn stage_of(room: &Arc<Room>, presets: &[&str]) -> Stage {
        Stage {
            room: room.clone(),
            translate: None,
            hands: std::sync::OnceLock::new(),
            presets: presets.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// A puppet is the first AI's and nobody else's, and only where the
    /// house can give one a voice. A bot that is only looking is never
    /// offered one, and nor is anybody in a house with nothing to speak
    /// with -- a tool it cannot use is a tool it will try.
    #[test]
    fn the_puppet_tools_belong_to_the_ai_that_runs_the_room() {
        let names = |allowed: Allowed, puppets: Option<&[String]>| {
            tools_for(
                allowed,
                true,
                false,
                false,
                true,
                &Art::default(),
                false,
                puppets,
            )
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
        };
        let every = [
            "list_puppets",
            "make_puppet",
            "forget_puppet",
            "bring_on_puppets",
            "send_off_puppets",
            "put_on_show",
            "say_as",
        ];

        let none = names(Allowed::Everything, None);
        for tool in every {
            assert!(
                !none.contains(&tool.to_string()),
                "{tool} with no speech server"
            );
        }
        let has = names(Allowed::Everything, Some(&[]));
        for tool in every {
            assert!(has.contains(&tool.to_string()), "{tool} is missing");
        }
        let looking = names(Allowed::Looking, Some(&[]));
        for tool in every {
            assert!(
                !looking.contains(&tool.to_string()),
                "{tool} is not a voice's"
            );
        }

        // The voices the server has go in the schema, so the model
        // picks one that exists rather than one it has heard of.
        let made = puppet_tools(false, &["Aiden".into(), "Serena".into()]);
        let make = made
            .iter()
            .find(|t| t["function"]["name"] == "make_puppet")
            .expect("make_puppet");
        let props = &make["function"]["parameters"]["properties"];
        assert!(
            props["preset"]["description"]
                .as_str()
                .unwrap()
                .contains("Aiden, Serena"),
            "{}",
            props["preset"]["description"]
        );
        assert!(
            props.get("voice").is_none(),
            "a cloned voice is only offered where there is a server to clone with"
        );
        assert!(
            puppet_tools(true, &[])[1]["function"]["parameters"]["properties"]["voice"].is_object()
        );
    }

    /// The prompt and the tools are decided by the same thing, so what
    /// the model is told it can do and what it is offered cannot drift
    /// apart. And a room with a puppet in it says which puppets, and
    /// whose they are.
    #[test]
    fn the_prompt_mentions_puppets_only_where_there_are_some() {
        let room = picture_room();
        let (agent, mut t) = turn_with(Cue::Speak, &["Ryan"], false);

        let quiet = system_of(&agent, &t);
        assert!(!quiet.contains("put on a show"), "no stage, nothing said");
        assert!(!quiet.contains("puppet"), "{quiet}");

        let stage = stage_of(&room, &["Aiden", "Serena"]);
        let said = agent.messages(&t, Some(&stage))[0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(said.contains("A puppet is a character"), "{said}");
        assert!(said.contains("Aiden, Serena"), "the voices it may build on");
        assert!(
            said.contains("you never answer one"),
            "the half a model gets wrong without being told"
        );
        assert!(
            !said.contains("In the room with you now"),
            "none are on stage"
        );

        // With a cast on stage, and then with the show playing.
        t.puppets = vec![("Grimble".into(), "an old gatekeeper".into())];
        let cast = agent.messages(&t, Some(&stage))[0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            cast.contains("In the room with you now: Grimble (an old gatekeeper)"),
            "{cast}"
        );
        assert!(cast.contains("lines you wrote"), "{cast}");

        t.showing = Some("The Gate".into());
        let playing = agent.messages(&t, Some(&stage))[0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            playing.contains("\"The Gate\" is playing right now"),
            "{playing}"
        );
        assert!(
            playing.contains("Say nothing and start nothing"),
            "{playing}"
        );
    }

    /// While a show is on, the AI is told that a whole part of the room
    /// is missing from the transcript it is reading -- and that is the
    /// thing it has to be told, far more than the layout. A model that
    /// does not know it cannot hear the audience will wonder at the
    /// silence, or apologise for ignoring somebody, or answer a
    /// question nobody put to it.
    #[test]
    fn the_director_is_told_it_cannot_hear_the_audience() {
        let room = picture_room();
        let (agent, mut t) = turn_with(Cue::Speak, &["Ryan"], false);
        let stage = stage_of(&room, &["aiden"]);
        let said = |t: &Turn| {
            agent.messages(t, Some(&stage))[0]["content"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // No show, nothing said: the room is a room.
        assert!(!said(&t).contains("playhouse"), "no show, no playhouse");

        // A show on, and nobody in the box or up on the stage: it is
        // reading a transcript with the audience cut out of it, and is
        // told so in those words.
        t.playhouse = true;
        let on = said(&t);
        assert!(on.contains("cannot hear the audience"), "{on}");
        assert!(on.contains("never address the audience"), "{on}");
        assert!(!on.contains("In the box with you"), "nobody has come round");
        assert!(
            !on.contains("adlib"),
            "nobody is up there, so it is still a script"
        );

        // Somebody comes round to the box: now there is one person it
        // can hear, and it is named.
        t.in_box = vec!["Ryan".into()];
        let box_ = said(&t);
        assert!(box_.contains("In the box with you: Ryan"), "{box_}");

        // And somebody goes up on the stage, which is the end of any
        // script and the start of making it up.
        t.on_stage = vec!["Alex".into()];
        let adlib = said(&t);
        assert!(adlib.contains("Alex is up on the stage"), "{adlib}");
        assert!(adlib.contains("there is no script"), "{adlib}");
        assert!(adlib.contains("say_as"), "{adlib}");
    }

    /// What the tools answer, and what they refuse: the roster, the
    /// stage, and a show that cannot go on without a cast.
    #[test]
    fn the_puppet_tools_say_what_went_wrong() {
        let store = crate::store::Store::memory().in_room("Lobby");
        store.invite("Ryan").unwrap();
        let tts: crate::room::VoiceFactory =
            Arc::new(|_, _, _, _| Ok(Box::new(crate::tts::Silent)));
        let (room, _wiring) = Room::new(
            store,
            RoomConfig {
                id: "Lobby".into(),
                title: "Lobby".into(),
                only: None,
                default_mode: Mode::Named,
                bots: vec![BotConfig::new("Vox", vec![])],
                reply_after: Duration::from_millis(1200),
                confirm: Duration::from_millis(300),
                engaged_for: Duration::from_secs(30),
                history: 60,
                ai_turns: 2,
                language: "English".into(),
                languages: vec!["English".into()],
                spoken: vec!["English".into()],
            },
            None,
            std::collections::BTreeMap::new(),
            None,
            Some(tts),
            None,
        )
        .unwrap();
        // Spelled as the server spells them, which is the only
        // spelling `/v1/audio/speech` takes.
        let stage = stage_of(&room, &["aiden", "uncle_fu"]);
        let call = |tool: &str, args: Value| puppeteering(&room, &stage, tool, &args);

        // Nothing yet, and the refusal says so rather than guessing.
        assert_eq!(
            call("list_puppets", json!({})).unwrap()["puppets"],
            json!([])
        );
        assert!(call("bring_on_puppets", json!({ "puppets": ["Grimble"] }))
            .unwrap_err()
            .contains("make_puppet makes one"));
        assert!(call("put_on_show", json!({ "title": "x", "script": [] }))
            .unwrap_err()
            .contains("bring_on_puppets first"));

        // A voice the server hasn't got is refused now, not in front of
        // the room.
        assert!(call(
            "make_puppet",
            json!({ "name": "Grimble", "about": "a gatekeeper", "preset": "Hamlet" })
        )
        .unwrap_err()
        .contains("the speech server has aiden, uncle_fu"));

        // Said the way anybody says it out loud, kept the way the
        // server spells it: those are two words for one voice, and
        // only one of them is a word the endpoint will take. This is
        // the whole of what went wrong the first time a puppet was
        // asked to speak -- "Uncle Fu" reached the server untouched
        // and came back HTTP 400 in the middle of a sentence.
        call(
            "make_puppet",
            json!({ "name": "Grimble", "about": "a gatekeeper", "preset": "Uncle Fu" }),
        )
        .unwrap();
        assert_eq!(
            room.puppet_roster()[0].preset,
            "uncle_fu",
            "spelled as the server spells it, whatever was said"
        );

        // A cast of one real name and one typo brings on what it can
        // and says which it could not: failing the lot would leave the
        // AI with no idea which of them is standing in the room.
        let on = call(
            "bring_on_puppets",
            json!({ "puppets": ["Grimble", "Grumble"] }),
        )
        .unwrap();
        assert_eq!(on["on_stage"], json!(["Grimble"]));
        assert_eq!(on["refused"][0]["puppet"], json!("Grumble"));

        // Now a show will go on, and a second one will not.
        let script = json!([{ "who": "Grimble", "say": "None shall pass." }]);
        let playing = call(
            "put_on_show",
            json!({ "title": "The Gate", "script": script }),
        );
        assert_eq!(playing.unwrap()["playing"], json!("The Gate"));
        assert!(call(
            "put_on_show",
            json!({ "title": "Again", "script": json!([{ "who": "Grimble", "say": "Hm." }]) })
        )
        .unwrap_err()
        .contains("a show is already on"));
        room.stop_show();

        // say_as is only for a puppet that is actually standing there.
        assert!(call("say_as", json!({ "who": "Pell", "say": "Hello." }))
            .unwrap_err()
            .contains("no puppet called \"Pell\""));
        assert!(call("say_as", json!({ "who": "Grimble", "say": "" }))
            .unwrap_err()
            .contains("say what it says"));
    }
}
