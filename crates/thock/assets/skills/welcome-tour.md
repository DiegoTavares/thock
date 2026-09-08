# Welcome Tour

_A ritual for the agent: give a brand-new Thock user their first ten minutes.
A human reading this: it's the script your agent follows when you take the
tour — edit it if you'd like the tour to go differently._

## Who you're talking to

Someone opening Thock for the very first time. They may never have used an
editor like this, and they may not know what an "agent" is beyond you. Be
warm, be brief, use plain words. One thing at a time — never a wall of text,
never two questions at once.

## Ground rules

- **Append, never rewrite.** You will add lines and sections; you will never
  reword or delete anything the user typed. (`profile.md` in step 3 is the
  one file written whole, and only after they've seen it.)
- **Ask, then wait.** Nothing is created or written without telling the user
  what you're about to do.
- Follow the vault conventions in `AGENTS.md` at the vault root.

## The tour, in order

1. **Say hello, and ask the language.** Introduce yourself in a sentence:
   you're their assistant, you live in this side panel, and together you'll
   set up their vault in a few minutes. Then one question before anything
   else: what language should the two of you speak? Wait.

   If the answer isn't English: read `skills/thock/set-language.md` and run
   that ritual now, start to finish (skip its opening question — you already
   know the answer), so the first note they see is already in their
   language. Then come back here and continue the tour in that language.

2. **Ask their name.** Wait.

3. **Ask what brought them, then write it down.** One light question: what
   do they hope Thock helps with: remembering their days? tasks?
   journaling? Wait for it.

   Then read `skills/thock/set-profile.md` and run that ritual now, start to
   finish (skip the question you just asked; you have the answer). It
   interviews them about their weeks and writes `profile.md`, which is what
   stops every later ritual from assuming they write code for a living: the
   rituals ship tuned for an engineer, and this is where that gets fixed.
   Keep the answers in mind for steps 4 and 7, then come back here.

4. **Open today's note.** Tell them every day gets its own page. On a
   brand-new vault today's note already exists and is a filled-in
   **example** (its first lines say so): sections, checkboxes, a few
   emojis, showing what a day can hold. Ask them to open it (the **Today**
   entry in the left rail), read it, and then replace the example lines
   under the planner heading (`## Day planner` — or its translated name, if
   step 1 set a language) with two or three things actually on their mind,
   each as a checkbox line: `- [ ] like this`. Point out the right-hand
   rail drawing their list as a day plan while they type. Mention once that
   tomorrow's page starts from `templates/daily.md`, which they (or you)
   can change any time.

   If today's note doesn't exist (an older vault, or they deleted it),
   create `daily/<today's date as YYYY-MM-DD>.md` from `templates/daily.md`
   (fill the template's `{{date:…}}` tokens with real values) and continue
   the same way.

5. **Show the one promise that matters.** Once they've written something,
   ask permission to add a short section to today's note. On a yes, append
   (at the end of the file, never touching their lines):

   ```
   ## Getting started

   - <their name> and <you, the agent> set up this vault together today.
   ```

   Then say it plainly: this is how you always work — you add your part
   below theirs, and you never rewrite what they wrote.

6. **Point at the rituals.** Explain the left rail's routine sections in a
   sentence or two: those verbs — Wrap Today, Week Review, Triage Inbox —
   are rituals; they run you, and each one is a readable file they can open
   and change.

7. **Offer one first ritual.** Based on the profile you wrote in step 3,
   offer to finish with a tiny **Wrap Today** (read `routines/timeline/skills/wrap-today.md`
   and run a gentle, short version — it's their first day, so there's little
   to wrap and that's fine). If they'd rather stop, that's a fine answer too.

8. **Finish.** Write the done marker: create the file
   `.thock/state/onboarded/welcome-tour` (make the folders if needed) with a
   one-line summary of what you did as its body. Tell them the Getting
   started list in the left rail will tick itself off, and that you're one
   keystroke away whenever they want you.

## If something goes wrong

Missing folders or notes are normal in a fresh vault — create what's needed
from `templates/` and carry on. If you can't finish the tour, still leave
the user with one concrete thing they can do next, and skip the done marker
so the tour stays offered.
