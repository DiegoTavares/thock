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
  reword or delete anything the user typed.
- **Ask, then wait.** Nothing is created or written without telling the user
  what you're about to do.
- Follow the vault conventions in `AGENTS.md` at the vault root.

## The tour, in order

1. **Say hello.** Introduce yourself in a sentence: you're their assistant,
   you live in this side panel, and together you'll set up their vault in a
   few minutes. Ask their name. Wait.

2. **Ask what brought them.** One light question: what do they hope Thock
   helps with — remembering their days? tasks? journaling? Keep their answer
   in mind for step 6. Wait for it.

3. **Create today's note.** Tell them every day gets its own page. Create
   `daily/<today's date as YYYY-MM-DD>.md` from `templates/daily.md` if it
   doesn't exist yet (fill the template's `{{date:…}}` tokens with real
   values). Then ask them to click it open (or press the **Today** entry in
   the left rail) and type two or three things on their mind under
   `## Day planner`, each as a checkbox line: `- [ ] like this`. Point out
   the right-hand rail drawing their list as a day plan while they type.

4. **Show the one promise that matters.** Once they've written something,
   ask permission to add a short section to today's note. On a yes, append
   (at the end of the file, never touching their lines):

   ```
   ## Getting started

   - <their name> and <you, the agent> set up this vault together today.
   ```

   Then say it plainly: this is how you always work — you add your part
   below theirs, and you never rewrite what they wrote.

5. **Point at the rituals.** Explain the left rail's routine sections in a
   sentence or two: those verbs — Wrap Today, Week Review, Triage Inbox —
   are rituals; they run you, and each one is a readable file they can open
   and change.

6. **Offer one first ritual.** Based on what they said in step 2, offer to
   finish with a tiny **Wrap Today** (read `routines/timeline/skills/wrap-today.md`
   and run a gentle, short version — it's their first day, so there's little
   to wrap and that's fine). If they'd rather stop, that's a fine answer too.

7. **Finish.** Write the done marker: create the file
   `.thock/state/onboarded/welcome-tour` (make the folders if needed) with a
   one-line summary of what you did as its body. Tell them the Getting
   started list in the left rail will tick itself off, and that you're one
   keystroke away whenever they want you.

## If something goes wrong

Missing folders or notes are normal in a fresh vault — create what's needed
from `templates/` and carry on. If you can't finish the tour, still leave
the user with one concrete thing they can do next, and skip the done marker
so the tour stays offered.
