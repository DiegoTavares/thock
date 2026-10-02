# Notes on the V34 API contract, from the phone port

Written while porting `thock/specs/v34-vault-sync-api.md` §7–§9 to Swift.

**Status, 2026-10-02: settled.** Items 1 to 4 below are amended in the contract on
`origin/v34-vault-sync` (§7.2, §8.3), with this port's `replace_line` rule adopted. The canonical
corpus is `crates/thock-sync-core/fixtures/v1` on that branch: 296 cases, 43 hash vectors, 3 envelope
vectors. The Swift port passes all of it, together with its own 41 cases. Until that branch is merged
into this one, run it with:

```sh
git archive origin/v34-vault-sync crates/thock-sync-core/fixtures | tar -x -C /tmp/desk
THOCK_SYNC_FIXTURES=/tmp/desk/crates/thock-sync-core/fixtures/v1 swift test
```

Once the crate is in the checkout the test finds it without the variable.

Three things changed here to match the corpus: a heading's closing hashes are stripped from its
text; `remove_line` against a missing file creates the heading-only file and reports `created`; and a
`replace_section` that replaces the unterminated last line of a file ends its new lines with a
terminator (an insertion after an unterminated line still keeps the file unterminated). The last one
reads oddly next to §8.1's "keeps that property for its last line" and is worth a sentence there.

The phone's own cases have not been run against the Rust crate; that needs this folder committed.

The original notes follow, as written before the amendments.

Each item says what the
contract says, what the phone does, and what to decide. Items 1 to 3 are places where following the
text literally breaks a promise the contract itself makes; the phone deviates there, and the fixtures
under `ThockKit/Tests/ThockKitTests/Fixtures/v1` pin the behaviour. Item 4 is a product problem the
phone follows the contract on anyway. The rest are ambiguities.

## Contradictions the phone resolves

**1. `replace_line` effect-present makes every tick a no-op (§8.3).** The table says the effect is
present when "a body line's `line_hash` equals `line_hash(new_line)`". `line_hash` ignores the
checkbox and the time prefix, so for a tick or a retime the old line already has the new line's hash,
rule 1 returns `noop`, and nothing is ever applied. The contract's own §9.2 example ("tick a line
the desk retimed meanwhile") expects `effect_present_before: false` and the line replaced.

The phone's rule: if the target (`line_hash`, `ordinal`) is found, the effect is present when that
line's text equals `new_line` (or `new_line` plus the marker), ignoring trailing whitespace. If the
target is not found, the effect is present when some body line has `line_hash(new_line)` or equals
`new_line` plus the marker. Fixtures: `replace_line/tick`, `retimed-meanwhile`, `duplicate-ordinal`,
`target-missing`.

**2. A `create` over different content is not idempotent (§8.2 rule 3, §8.3).** Rule 3 turns it into
an append, but `create`'s effect is present only when the file equals `content`, so a re-applied
create (a desk crash between apply and ack) appends the content again. The phone also treats the
effect as present when the file contains `content`'s lines as a contiguous run, the same test an
append gets. Fixture: `create/existing-different`.

**3. `remove_line` with duplicate lines is not idempotent (§8.3).** "No body line has `line_hash`"
is still false after removing one of two identical lines, so a re-apply removes the other one too.
There is no stateless fix. The phone follows the text; a desk crash between apply and ack can remove
both copies of a duplicated line. Worth a sentence in the contract, or a count in the write.

## A product problem the phone follows the contract on

**4. Appends land after the `___` that ends every template section (§7.2, §8.2 rule 5).** A section's
body is everything up to the next heading "with trailing blank lines removed". The shipped daily
template, and the example day, end each section with a rule:

```markdown
## Journal

_What happened, what you noticed, how it went._

___

## Day planner
```

so a journal entry, or a planner task, appended at `end` or `before_children` lands *below* the
rule, directly above the next heading:

```markdown
**13:02** · Noticed I keep saying yes to things on Thursdays.

___

**13:29** · Walked past the bakery.

## Day planner
```

On the desk that reads as belonging to nothing, and it breaks V33's "nobody at the desk can tell
which lines came from the phone". Proposed wording for §7.2: *its body is that range with trailing
blank lines, then one trailing thematic break (`___`, `---` or `***`), then trailing blank lines again,
removed.* It is one function on each side (`TextFile.range` here). The phone implements the contract
as written until this is decided, because the desk must do the same thing.

## Ambiguities, and what the phone does

5. **`before_children` (§7.3, §8.2 rule 5).** "After the last of the own lines": the phone trims the
   own lines' trailing blank lines first, as the body's are, so a task lands under the last loose
   task and not after the blank line above the first category. Fixture: `append/before-children`.
6. **A missing heading inserted before a level-1 heading (§8.2 rule 4).** The phone inserts a blank
   line after the new section too when it goes before an existing heading, so `# Daily Closure` does
   not hug the appended lines. At the end of the file it adds none. Fixtures:
   `append/section-missing-*`.
7. **A missing file, no template (§8.2 rule 2).** "The heading line followed by a blank line, then
   continue": the appended lines go between the heading and that blank line, since the body is empty
   and a section's trailing blank lines stay after inserted lines. Result: `## Day planner\n- [ ] A\n\n`.
   Fixture: `append/missing-file`.
8. **Outcome precedence.** When more than one applies the phone reports `kept_both`, then `created`,
   then `section_added`, then `applied`.
9. **Heading `ordinal` (§7.2).** It picks among the exact lowercase matches when there are any, and
   among the key matches only when there are none.
10. **Time prefix (§7.4 step 4).** The phone follows the V4 grammar as the desk's `day_plan.rs`
    implements it: hours 0–23, minutes 0–59, `24:00` allowed as a range end, and the time must be
    followed by whitespace or the end of the line (`9:30am call` keeps its `9:30am`). Vectors in
    `hashes.json`.
11. **Heading text.** `^#{1,6}[ \t]+\S`, text trimmed, closing hashes left in (the key folds them
    away). A heading with leading spaces is not a heading here, though the desk's
    `heading_level_and_text` accepts one.
12. **Template seeds can differ.** The phone expands `{{time}}` with the capture's clock; the desk
    will expand it with its own. If a template uses `{{time}}` the two created notes differ by that
    token until the desk's snapshot arrives, which then wins. No action needed, noted so it is not
    a surprise.
13. **Envelope vector contexts.** `envelope.json`'s `context` object is not specified; the phone's
    file uses `{"kind": "file", "path", "blob_id"}` and `{"kind": "write", "client_id"}`.

## What the phone needs from the other sessions

- **Backend:** nothing beyond the contract. `LocalBackend` is this port's reading of §6 and may be a
  useful cross-check for the Go tests (status codes, `current` on 409, idempotent writes by
  `client_id`, feed to the other device only).
- **Desk:** the fixtures directory, and a decision on items 1 to 4. The phone's journal recognises
  `## Journal` by its heading key only; the vault config has no journal heading, so a translated
  vault (V19) needs one, or the phone needs the template to tell it.
