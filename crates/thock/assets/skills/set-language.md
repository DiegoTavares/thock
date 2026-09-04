# Set Language

> A ritual for the agent, launched from **thock: set language** in the
> command bar (and by the Welcome Tour when the user picks a language other
> than English). A human reading this: it's the script your agent follows to
> move your vault into your language — edit it if you'd like it done
> differently.

You are helping the user make this vault speak their language: the
templates, the docs, the parsed section headings, and every word you say.
The app's own buttons and panels stay in English for now — say that plainly
rather than hiding it.

## Ground rules

- Follow `AGENTS.md` at the vault root. Renaming headings and retitling
  templates (steps 5–6) rewrites shipped files the user asked to have
  rewritten — the explicit-request exception to "append, never rewrite" —
  so say what you're changing as you change it.
- **Confirm before each write batch.** Show what will change, wait for a
  yes.
- The only paths you may touch under `.thock/` are `.thock/config.toml`
  (step 4, the sanctioned settings write) and the done marker (step 8).
- **Never touch the user's own notes** (`daily/`, `weekly/`, `backlog.md`
  task lines, anything they wrote). What they wrote stays in the language
  they wrote it in; only headings the app parses and shipped prose change.
- Keep unchanged, in every file: file and folder names, paths and
  wikilinks, the task syntax (`- [ ]`), `{{date:…}}` template tokens, and
  anything inside code or config blocks.

## The ritual

### 1. Ask

One question: **what language should I use with you?** Accept the answer in
the user's own words ("português", "Brazilian Portuguese", "français").
If you were launched already knowing the answer (the Welcome Tour asks
first), skip the question.

If the answer is English, write only the `[language]` table from step 4
(so a re-run knows), tell the user there's nothing to translate, and stop.

From here on, speak the chosen language.

### 2. Explain the shape, in one breath

One sentence: your notes and I will be in <language>; the app's own buttons
are still in English for now.

### 3. Bind yourself first

Append to `AGENTS.md` — a new section at the end, never editing what's
above it — so every later step, and every future session, is already in
the right language:

```markdown
## Language

Speak and write in **<Language (Region)>** — in conversation, and in
everything you append to a note. Keep unchanged: file and folder names, the
marker paths under `.thock/`, and the task syntax (`- [ ]`). Section
headings the app parses are named in `.thock/config.toml`; follow that
file, not your memory.
```

(If a `## Language` section already exists, this is a re-run: rewrite that
one section to the new language.)

Then propose the whole plan and get a yes: the config keys to be written —
`[language]`, `[backlog] headings`, `[day_planner] heading` — with your
proposed translations of **Soon**, **Someday**, **Completed**, and
**Day planner**; the heading renames in `backlog.md` and
`templates/daily.md`; and the files whose prose will be translated
(step 6's list).

### 4. Write the settings

In `.thock/config.toml` — merge into what's already there, keeping every
existing key:

```toml
[language]
tag  = "pt-BR"                   # the BCP 47 tag for the chosen language
name = "Portuguese (Brazil)"     # the language, in the user's words

[backlog]
headings = { soon = "…", someday = "…", completed = "…" }

[day_planner]
heading = "…"
```

### 5. Rename the headings that are parsed

To exactly what step 4 wrote — the app matches these strings:

- `backlog.md` — the three section headings. Only the heading lines; every
  task and line under them stays untouched.
- `templates/daily.md` — the planner heading (the `[day_planner] heading`
  value).

### 6. Translate the prose, in confirmed batches

One batch at a time, confirming each before writing:

1. `templates/daily.md` and `templates/weekly.md` — headings and prose.
   Retitle the daily template to the wordless `# {{date:YYYY-MM-DD}}`:
   the `MMMM`/`dddd` tokens produce English month and weekday names, and a
   plain date reads the same in every language. Say that one line of why.
2. `welcome.md` and `guide/customize.md`.
3. Each installed Routine's explainer doc — read `routines/*/routine.toml`
   for the `doc` entries.

### 7. Offer the extras

Ask — never do this silently: the skill files under `skills/` and
`routines/*/skills/`, and the guide `guide/index.html`, can be translated
too. Warn first: **Thock improves its shipped skills over time; a
translated copy becomes the user's own and stops receiving those updates.**
Translate only what they say yes to.

### 8. Complete

Write `.thock/state/onboarded/set-language` (create the folders if needed)
with a one-line summary of what changed as its body. Tell the user this
ritual can be re-run any time — command bar (`cmd-shift-p`), **thock: set
language** — including to change languages again or go back to English.

## If something goes wrong

Missing files are normal in a hand-edited vault — skip what isn't there and
say so. If you can't finish, leave `.thock/config.toml`, `AGENTS.md`, and
the renamed headings consistent with each other (the app falls back to the
English headings while they disagree, so nothing breaks), and tell the user
exactly what remains.
