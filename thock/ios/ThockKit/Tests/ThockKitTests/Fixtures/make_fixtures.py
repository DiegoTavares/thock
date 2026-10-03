#!/usr/bin/env python3
"""Writes the phone-authored fixture cases in the V34 API §9.2 format.

Expected texts are written out by hand from the contract; only the hashes are
computed, from the identity each line is expected to reduce to. Run from this
directory: `python3 make_fixtures.py`.
"""
import hashlib
import json
import os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "v1")
MARKER = " <!--thock:also-->"


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def base(kind, path="daily/2026-10-02.md"):
    return {
        "v": 1,
        "client_id": "0f7e0b1a-3c4d-4e5f-8a9b-0c1d2e3f4a5b",
        "kind": kind,
        "path": path,
        "made_at": "2026-10-02T13:58:02Z",
        "device_id": "c41a9e0d7b2f4a61",
    }


def heading(text, level=2, ordinal=0):
    return {"text": text, "level": level, "ordinal": ordinal}


def append(head, lines, **extra):
    write = base("append")
    write["heading"] = head
    write["lines"] = lines
    write.update(extra)
    return write


def replace_line(head, identity, new_line, ordinal=0):
    write = base("replace_line")
    write.update({"heading": head, "line_hash": digest(identity), "ordinal": ordinal, "new_line": new_line})
    return write


def remove_line(head, identity, ordinal=0):
    write = base("remove_line")
    write.update({"heading": head, "line_hash": digest(identity), "ordinal": ordinal})
    return write


def replace_section(head, base_body, lines):
    write = base("replace_section")
    write.update({"heading": head, "base_hash": digest("\n".join(base_body)), "lines": lines})
    return write


def create(path, content):
    write = base("create", path)
    write["content"] = content
    return write


PLANNER = heading("Day planner")
CASES = []


def case(area, slug, name, before, write, after, outcome, present_before=False, present_after=True, seed=None):
    CASES.append((area, slug, {
        "name": name,
        "before": before,
        "seed": seed,
        "write": write,
        "after": after,
        "outcome": outcome,
        "effect_present_before": present_before,
        "effect_present_after": present_after,
    }))


case("append", "journal-entry", "a journal entry lands at the end of its section, blank line kept after it",
     "# T\n\n## Journal\n\nFirst.\n\n## Day planner\n- [ ] A\n",
     append(heading("Journal"), ["**21:14** · New"], blank_line_before=True),
     "# T\n\n## Journal\n\nFirst.\n\n**21:14** · New\n\n## Day planner\n- [ ] A\n", "applied")

case("append", "planner-task", "a task lands after the planner's last line",
     "## Day planner\n- [ ] A\n\n## Personal\n",
     append(PLANNER, ["- [ ] B"]),
     "## Day planner\n- [ ] A\n- [ ] B\n\n## Personal\n", "applied")

case("append", "before-children", "a backlog task lands below the loose tasks, above the first category",
     "## Soon\n\n- [ ] Loose\n\n### OpenCue\n\n- [ ] Fix\n\n## Someday\n",
     append(heading("Soon"), ["- [ ] New"], placement="before_children"),
     "## Soon\n\n- [ ] Loose\n- [ ] New\n\n### OpenCue\n\n- [ ] Fix\n\n## Someday\n", "applied")

case("append", "null-heading", "a null heading is the end of the file",
     "Line\n",
     append(None, ["Tail"], blank_line_before=True),
     "Line\n\nTail\n", "applied")

case("append", "missing-file", "a missing file becomes the heading, a blank line, and the lines",
     None,
     append(PLANNER, ["- [ ] A"]),
     "## Day planner\n- [ ] A\n\n", "created")

case("append", "missing-file-from-template", "a missing daily note is created from the expanded template",
     None,
     append(heading("Journal"), ["**08:00** · Hi"], blank_line_before=True, create_from_template=True),
     "# Friday\n\n## Journal\n\n_Prompt._\n\n**08:00** · Hi\n\n## Day planner\n", "created",
     seed="# Friday\n\n## Journal\n\n_Prompt._\n\n## Day planner\n")

case("append", "already-there", "lines already in the section are not appended twice",
     "## Day planner\n- [ ] A\n- [ ] B\n",
     append(PLANNER, ["- [ ] B"]),
     "## Day planner\n- [ ] A\n- [ ] B\n", "noop", present_before=True)

case("append", "section-missing-before-agent-heading", "a missing section goes before the agent's level-1 heading",
     "# Day\n\n## Journal\n\nText.\n\n# Daily Closure\n\nDone.\n",
     append(PLANNER, ["- [ ] A"]),
     "# Day\n\n## Journal\n\nText.\n\n## Day planner\n- [ ] A\n\n# Daily Closure\n\nDone.\n", "section_added")

case("append", "section-missing-end-of-file", "a missing section goes to the end of the file",
     "# Day\n\nText.\n",
     append(PLANNER, ["- [ ] A"]),
     "# Day\n\nText.\n\n## Day planner\n- [ ] A\n", "section_added")

case("append", "above-the-closing-rule", "a section's closing rule is not part of its body, so an entry lands above it",
     "## Journal\n\n_Prompt._\n\nFirst.\n\n___\n\n## Day planner\n",
     append(heading("Journal"), ["**21:14** · New"], blank_line_before=True),
     "## Journal\n\n_Prompt._\n\nFirst.\n\n**21:14** · New\n\n___\n\n## Day planner\n", "applied")

case("append", "task-above-the-closing-rule", "a task lands under the last task, above the rule",
     "## Day planner\n\n- [ ] A\n\n___\n\n## Personal\n",
     append(PLANNER, ["- [ ] B"], placement="before_children"),
     "## Day planner\n\n- [ ] A\n- [ ] B\n\n___\n\n## Personal\n", "applied")

case("append", "run-cannot-span-a-fence", "lines inside a fence do not count as already present",
     "## Day planner\n```\n- [ ] B\n```\n",
     append(PLANNER, ["- [ ] B"]),
     "## Day planner\n```\n- [ ] B\n```\n- [ ] B\n", "applied")

case("create", "new-file", "create writes the content when the file is missing",
     None, create("inbox/2026-10-02-2114-sunday.md", "# Sunday\n\nNo plans.\n"),
     "# Sunday\n\nNo plans.\n", "created")

case("create", "same-content", "create over identical content does nothing",
     "# Sunday\r\n\r\nNo plans.\r\n", create("inbox/2026-10-02-2114-sunday.md", "# Sunday\n\nNo plans.\n"),
     "# Sunday\r\n\r\nNo plans.\r\n", "noop", present_before=True)

case("create", "existing-different", "create over different content appends, so a capture is never lost",
     "Old\n", create("inbox/2026-10-02-2114-sunday.md", "New line\n"),
     "Old\n\nNew line\n", "applied")

case("replace_line", "tick", "a tick replaces the one line",
     "## Day planner\n- [ ] 09:30 - 11:00 Deep work\n- [ ] Lunch\n",
     replace_line(PLANNER, "Deep work", "- [x] 09:30 - 11:00 Deep work"),
     "## Day planner\n- [x] 09:30 - 11:00 Deep work\n- [ ] Lunch\n", "applied")

case("replace_line", "retimed-meanwhile", "tick a line the desk retimed meanwhile",
     "## Day planner\n- [ ] 09:30 - 11:00 Deep work\n",
     replace_line(PLANNER, "Deep work", "- [x] Deep work"),
     "## Day planner\n- [x] Deep work\n", "applied")

case("replace_line", "target-missing", "a line the desk reworded is kept beside the phone's version",
     "## Day planner\n- [ ] Lunch\n\n## Personal\n",
     replace_line(PLANNER, "Deep work", "- [x] Deep work"),
     "## Day planner\n- [ ] Lunch\n- [x] Deep work" + MARKER + "\n\n## Personal\n", "kept_both")

case("replace_line", "duplicate-ordinal", "the ordinal picks among identical lines",
     "## Day planner\n- [ ] Buy milk\n- [ ] Buy milk\n",
     replace_line(PLANNER, "Buy milk", "- [x] Buy milk", ordinal=1),
     "## Day planner\n- [ ] Buy milk\n- [x] Buy milk\n", "applied")

case("replace_line", "edit-text", "an edit keeps the checkbox and time and changes only the words",
     "## Day planner\n- [ ] 15:00 - 15:30 Call dentist\n- [ ] Lunch\n",
     replace_line(PLANNER, "Call dentist", "- [ ] 15:00 - 15:30 Call the dentist about Friday"),
     "## Day planner\n- [ ] 15:00 - 15:30 Call the dentist about Friday\n- [ ] Lunch\n", "applied")

case("replace_line", "trailing-comment", "a trailing marker does not change a line's identity",
     "## Day planner\n- [ ] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n",
     replace_line(PLANNER, "Standup", "- [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->"),
     "## Day planner\n- [x] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->\n", "applied")

case("replace_line", "fenced-lines-ignored", "a line inside a code fence is never the target",
     "## Day planner\n```\n- [ ] Deep work\n```\n- [ ] Deep work\n",
     replace_line(PLANNER, "Deep work", "- [x] Deep work"),
     "## Day planner\n```\n- [ ] Deep work\n```\n- [x] Deep work\n", "applied")

case("replace_line", "missing-file", "a line edit against a missing file creates it, and created outranks kept_both",
     None,
     replace_line(PLANNER, "Deep work", "- [x] Deep work"),
     "## Day planner\n- [x] Deep work" + MARKER + "\n\n", "created")

case("replace_line", "null-heading", "a null heading addresses the whole file",
     "- [ ] A\n- [ ] B\n",
     replace_line(None, "B", "- [x] B"),
     "- [ ] A\n- [x] B\n", "applied")

case("remove_line", "found", "remove deletes the one line",
     "## Day planner\n- [ ] A\n- [ ] B\n- [ ] C\n",
     remove_line(PLANNER, "B"),
     "## Day planner\n- [ ] A\n- [ ] C\n", "applied")

case("remove_line", "missing", "removing a line that is already gone does nothing",
     "## Day planner\n- [ ] A\n",
     remove_line(PLANNER, "B"),
     "## Day planner\n- [ ] A\n", "noop", present_before=True)

case("replace_section", "base-matches", "a section rewrite replaces only the body",
     "## Personal\n\nOld one.\nOld two.\n\n## Next\nKeep.\n",
     replace_section(heading("Personal"), ["", "Old one.", "Old two."], ["", "New."]),
     "## Personal\n\nNew.\n\n## Next\nKeep.\n", "applied")

case("replace_section", "stale-base", "a stale section rewrite is appended below the current one",
     "## Personal\n\nDesk version.\n\n## Next\n",
     replace_section(heading("Personal"), ["", "Older version."], ["Phone version.", "Second line."]),
     "## Personal\n\nDesk version.\n\nPhone version." + MARKER + "\nSecond line.\n\n## Next\n", "kept_both")

case("replace_section", "keeps-the-closing-rule", "a section rewrite leaves the closing rule where it is",
     "## Personal\n\nOld.\n\n___\n\n## Next\n",
     replace_section(heading("Personal"), ["", "Old."], ["", "New."]),
     "## Personal\n\nNew.\n\n___\n\n## Next\n", "applied")

case("replace_section", "stale-base-marks-first-words", "the marker goes on the first non-blank line that was appended",
     "## Personal\n\nDesk version.\n\n## Next\n",
     replace_section(heading("Personal"), ["", "Older version."], ["", "Phone version."]),
     "## Personal\n\nDesk version.\n\n\nPhone version." + MARKER + "\n\n## Next\n", "kept_both")

case("headings", "unclosed-front-matter", "front matter that never closes is ordinary content",
     "---\n## Day planner\n- [ ] A\n",
     append(PLANNER, ["- [ ] B"]),
     "---\n## Day planner\n- [ ] A\n- [ ] B\n", "applied")

case("headings", "decorated", "a decorated heading still matches by its key",
     "## 📅 **Day planner**:\n- [ ] A\n",
     append(PLANNER, ["- [ ] B"]),
     "## 📅 **Day planner**:\n- [ ] A\n- [ ] B\n", "applied")

case("headings", "exact-wins", "an exact heading wins over a normalised one, whatever the order",
     "## Day-planner\n- [ ] X\n\n## Day planner\n- [ ] Y\n",
     append(PLANNER, ["- [ ] Z"]),
     "## Day-planner\n- [ ] X\n\n## Day planner\n- [ ] Y\n- [ ] Z\n", "applied")

case("headings", "not-in-fence-or-frontmatter", "headings inside frontmatter and fences are not headings",
     "---\n# Day planner\ntitle: x\n---\n\n```\n## Day planner\n```\n\n## Day planner\n- [ ] A\n",
     append(PLANNER, ["- [ ] B"]),
     "---\n# Day planner\ntitle: x\n---\n\n```\n## Day planner\n```\n\n## Day planner\n- [ ] A\n- [ ] B\n", "applied")

case("headings", "level-ignored", "the level is ignored when matching",
     "### Day planner\n- [ ] A\n",
     append(PLANNER, ["- [ ] B"]),
     "### Day planner\n- [ ] A\n- [ ] B\n", "applied")

case("line_endings", "crlf", "inserted lines use the file's line ending",
     "## Day planner\r\n- [ ] A\r\n",
     append(PLANNER, ["- [ ] B"]),
     "## Day planner\r\n- [ ] A\r\n- [ ] B\r\n", "applied")

case("line_endings", "no-final-newline", "a file without a final newline stays without one",
     "## Day planner\n- [ ] A",
     append(PLANNER, ["- [ ] B"]),
     "## Day planner\n- [ ] A\n- [ ] B", "applied")

ROUNDTRIP = {
    "frontmatter-and-table": "---\ntitle: x # not a comment\n---\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n<!-- note -->\n",
    "mixed-endings": "one\r\ntwo\nthree\r\n\r\nfour",
    "nested-lists": "- a\n  - b\n    - [ ] c\n\n1. one\n2) two\n\n> quote\n> more\n\n___\n",
    "code-fence": "```rust\n## not a heading\nfn main() {}\n```\n\n~~~\n- [ ] not a task\n~~~\n",
}
for slug, text in ROUNDTRIP.items():
    CASES.append(("roundtrip", slug, {"name": slug.replace("-", " "), "before": text, "seed": None, "write": None,
                                      "after": text, "outcome": "noop",
                                      "effect_present_before": False, "effect_present_after": False}))

LINES = [
    ("- [ ] 09:30 - 11:00 Deep work", "Deep work"),
    ("- [x] Deep work", "Deep work"),
    ("  * [X] 9:05–10:00   Stand  up  ", "Stand up"),
    ("- [ ] 10:00 - 10:30 Standup <!--gcal:aaaaaaaaaaaa-->", "Standup"),
    ("- [ ] Lunch <!--gcal:x--> <!--thock:also-->", "Lunch"),
    ("3. Third item", "Third item"),
    ("- [ ] 14:00 to 15:00 Review", "Review"),
    ("- [ ] 23:00 - 24:00 Late", "Late"),
    ("**21:14** · Walked past the bakery", "**21:14** · Walked past the bakery"),
    ("- [ ] 9:30am call", "9:30am call"),
    ("- [ ] 25:00 Not a time", "25:00 Not a time"),
    ("+ [ ] 08:00 Morning walk 🚶", "Morning walk 🚶"),
    ("Plain paragraph", "Plain paragraph"),
]
KEYS = [
    ("📅 **Day planner**:", "day planner"),
    ("Day-planner", "day planner"),
    ("[Day planner](plan.md)", "day planner"),
    ("[[Day planner]]", "day planner"),
    ("[[plan|Day planner]]", "day planner"),
    ("Day planner ##", "day planner"),
    ("Planejamento do dia", "planejamento do dia"),
    ("日次計画", "日次計画"),
    ("Week 40, 2026", "week 40 2026"),
]

for area, slug, body in CASES:
    folder = os.path.join(ROOT, area)
    os.makedirs(folder, exist_ok=True)
    with open(os.path.join(folder, slug + ".json"), "w", encoding="utf-8") as handle:
        json.dump(body, handle, ensure_ascii=False, indent=2)
        handle.write("\n")

hashes = [{"line": line, "line_hash": digest(identity)} for line, identity in LINES]
hashes += [{"text": text, "heading_key": key} for text, key in KEYS]
with open(os.path.join(ROOT, "hashes.json"), "w", encoding="utf-8") as handle:
    json.dump(hashes, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
print(len(CASES), "cases,", len(hashes), "hash vectors")
