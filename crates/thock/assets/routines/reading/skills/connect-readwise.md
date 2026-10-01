# Connect Readwise

Link the user's Readwise account to this vault so their highlights — Kindle, articles, podcasts — land as plain Markdown notes under the folders `.thock/readwise.toml` maps (`reference/readwise/books/` by default), one note per book, new highlights appended as they appear. Thock itself does the syncing; this skill's job is to get the connection made and explain what the user will see.

**Reads:** `.thock/readwise.toml` (to explain the current map).
**Writes:** nothing directly. Thock writes `.thock/readwise.toml` with the defaults the first time a token is accepted; you only ever suggest edits to it for the user to make.

> **Do not handle the token yourself.** The prompt runs inside Thock (system browser + system keychain); no token is ever written to the vault, and there is nothing for you to fetch, validate, or store. Never ask the user to paste a token into the chat. Your role is to start the flow and explain the rules.

## 1. Start the connection

1. Ask the user to run **`thock: connect readwise`** from the command palette (or click **Connect Readwise** on the Readwise row — behind the sync icon in the bottom bar, and at the top of the Backlog panel until it's connected).
2. Their browser opens `readwise.io/access_token`. They copy the token, paste it into Thock's prompt, and press `enter`. Thock checks it against Readwise, keeps it in the keychain, and writes `.thock/readwise.toml` if it doesn't exist yet.
3. Within a minute `reference/readwise/books/` fills with one note per book. From then on, a highlight made on the Kindle shows up in its book's note within one poll (hourly by default).
4. That's it. The sync icon's Readwise row says `synced …` once the first import is done, and the row leaves the Backlog panel.

## 2. Explain the note format (only if asked)

- Each book is one note: `source: readwise` / `readwise_id` / `category` in the frontmatter, then the Readwise Obsidian plugin's familiar body — a `## Metadata` block with the author as a `[[wikilink]]`, and a `## Highlights` list.
- Every highlight line ends with an invisible comment, `<!--rw:<id>@<YYYY-MM-DD>-->`: the highlight's identity and the day it was made. The editor hides it. Tell the user to leave it in place; everything else on the line is theirs to reword.
- Highlight notes appear as a `- Note:` sub-bullet, highlight tags as `- Tags:`. Location links follow the source: Kindle locations open the book, pages are plain, podcast timestamps link to the episode.

## 3. Explain the rules (only if asked)

- **Append-only.** After a note is created, Thock only ever inserts new highlight lines at the end of `## Highlights`. It never rewords, reorders, or deletes anything, including its own earlier lines.
- **Deleted stays deleted.** A highlight line the user removes is never appended again. A deleted note comes back only with highlights made after the deletion.
- **Read-only toward Readwise.** Nothing is created, tagged, or changed there; edits and deletions made in Readwise later don't reach lines already landed.
- **Late highlights land at the end**, even when they sit earlier in the book.

## 4. Adjust preferences (on request)

`.thock/readwise.toml` is plain TOML the user edits (you may propose the exact lines, but under AGENTS.md rule 3 you don't write under `.thock/` yourself):

- One `[[sync]]` entry per category to sync — `books`, `articles`, `tweets`, `podcasts`, `supplementals` — each with the folder its notes land in. Unmapped categories are skipped. Two entries for the same category is an error the Backlog row reports.
- `poll_minutes` (15–1440, default 60).

Deleting the file turns the sync off and leaves every note in place. **`thock: disconnect readwise`** also forgets the token.

## 5. If something is off

- The row says **token rejected**: the token was revoked or regenerated on readwise.io. Run `thock: connect readwise` again.
- The row names a config problem (`two entries for "books"`): fix the line it names in `.thock/readwise.toml`; the sync resumes on save.
- A book the user expected is missing: Readwise skips highlights marked as discarded, and only the mapped categories land. Articles and podcasts need their own `[[sync]]` entry.
