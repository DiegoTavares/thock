# Set Profile

_A ritual for the agent: find out who this person is and what they actually
want tracked, then write it down where every other ritual will read it. A
human reading this: it's the interview behind `profile.md`. Edit this file
if you'd like to be asked different questions, or just edit `profile.md`
directly and skip the interview forever._

## Why this exists

Thock's rituals ship with a default shape, and the default shape was written
by someone who writes code for a living. Left alone, **Wrap Today** goes
looking for commits and **Week Review** counts pull requests: useful for an
engineer, noise for a teacher, a nurse, a founder, or a parent running a
household. This ritual replaces the guess with an answer.

The answer lives in **`profile.md` at the vault root**. Every other ritual
reads it before it runs.

## Ground rules

- **Ask, then wait.** One question at a time. Never a wall of text, never
  two questions at once.
- **Plain words.** Unless the user turns out to be an engineer, no
  engineering vocabulary. Not "repository", not "sync", not "config".
- **Nothing is written until the user has seen it.** Show the profile you
  intend to write, in full, and get a yes.
- **Their words, not yours.** The areas in the profile should read the way
  the user said them ("the diploma", "Mum's appointments"), not the way you
  would have categorized them.
- Follow the vault conventions in `AGENTS.md`.

## The interview

Skip any question you already know the answer to from this conversation.
The Welcome Tour and the Timeline setup both call this ritual after they've
already asked some of it. Say what you already have and move on.

1. **Who they are.** What they do with their days: a job, a course, a
   business, caring for people, some mix. Wait.

2. **What they want from Thock.** What they hope this helps with. Let it be
   vague; "remembering things" is a real answer. Wait.

3. **What their weeks are actually made of.** Ask them to name the handful
   of things their time goes into. Four to six is plenty. Wait.

   These become the **areas**: the names the weekly review groups work
   under and the dashboard charts. Keep them short and stable; you will be
   reusing them every week.

4. **What you may go and look at.** Explain it in one sentence: besides
   what they write in their notes, you can pull a few things in
   automatically, and you'll only ever look at what they list here. Then
   offer the list and let them pick:

   - **Code.** Commits and pull or merge requests from repositories they
     name. Offer this **only if** the answers so far suggest they write
     code. If nothing in the interview mentioned programming, don't raise
     it; a person who writes code will say so.
   - **Calendar.** Meetings and appointments (needs the Connect Google
     Workspace setup; mention it only as a next step, don't start it here).
   - **Email.** Labelled mail becoming tasks (same setup, same caveat).

   Default everything to off. Off means *never look and never ask again*.

5. **Tone.** One last light question: how should you talk to them? Plain
   and warm, brief and businesslike, something else? Wait.

## Writing the profile

Show the whole file first, get a yes, then write `profile.md` at the vault
root. Use exactly these headings, because the other rituals look them up by
name:

```markdown
# About you

_The rituals read this page before they run, so they ask about your life
instead of someone else's. It's a normal note: open it and change anything._

## Who you are

One or two sentences, in their words.

## What you want from Thock

- One line per hope, from question 2.

## What you track

- One line per area, from question 3.

## What Thock may pull in

- [ ] Code (commits, pull requests, merge requests)
- [ ] Calendar (meetings and appointments)
- [ ] Email (labelled mail)

An unchecked box means **don't look and don't ask**. A ritual that finds a
box unchecked skips that step in silence.

## Tone

One line, from question 5.
```

Check the boxes the user chose in question 4 and leave the rest unchecked.
Keep every heading even when a section is short; the rituals match on them.

If `profile.md` already exists, read it first, show the user what would
change, and rewrite it only on a yes. This is the one file this ritual owns,
so a rewrite is allowed here; nowhere else.

## Making the rest of the vault match

Do each of these only for the parts of the vault that exist. A missing file
or an uninstalled Routine is normal: skip it and say nothing.

1. **Settle the code question for good.** When the Timeline Routine is
   installed (`routines/timeline/` exists) write
   `routines/timeline/sources.md` so the wrap and review rituals stop asking:

   - **Code unchecked.** Write the file with both lists explicitly empty:

     ```markdown
     # Timeline Sources

     Where the Wrap and Week Review rituals look for work outside your
     notes. Edit freely.

     ## Local checkouts

     _None._

     ## Forges

     _None. This vault doesn't track code._
     ```

     That is an **answer**, not an empty file. The rituals treat it as
     settled and never raise repositories again.

   - **Code checked.** Ask which repositories (local folder paths, or a
     hosted account as host + username), and record them under those two
     headings in the same format. If they don't know yet, write the file
     with both lists empty and a line saying the user will fill it in;
     don't leave it missing.

2. **Tell the weekly dashboard.** When `weekly/site/data.js` exists, set (or
   add, just above `window.WEEKS`) the profile block:

   ```js
   window.PROFILE = {
     code: false,                       // true when "Code" is checked
     focus: ["Area one", "Area two"]    // "What you track", in order
   };
   ```

   With `code: false` the dashboard drops the pull-request panel, its stat
   tiles, and its timeline bar, and shows carried-over goals and personal
   items in their place. `focus` keeps each area the same colour from week
   to week. Leave `window.WEEKS` and everything below it untouched, then
   check the file still parses:

   ```bash
   node -e "global.window={};require('./weekly/site/data.js');console.log(window.WEEKS.length,'weeks')"
   ```

3. **Leave the rituals alone.** Don't edit the skill files to match the
   profile; they already read it. Editing them is the user's privilege,
   not yours.

## Finish

Tell the user, in two or three sentences:

- what you wrote and where (`profile.md`, and the two files above if you
  touched them),
- that the rituals will now ask about their life rather than someone
  else's,
- that they can open `profile.md` and change it any time, or run this again
  (`thock: set profile`) when their weeks change shape.

Then create `.thock/state/onboarded/profile` (make the folders if needed)
with a one-line summary as its body, so the app knows the profile has been
set once.
