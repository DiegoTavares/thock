---
name: zed-upstream-review
description: >-
  Review upstream Zed for fixes worth porting into Thock, which is a hard fork
  that never merges upstream. Use when Zed publishes a stable release, when a
  Zed security advisory appears, or when Diego asks what upstream has that
  Thock should take. Ends in a review PR on DiegoTavares/thock carrying the
  chosen cherry-picks and an Upstream-Reviewed-Through trailer.
---

# Zed upstream review

Thock is a hard fork of Zed (`thock/specs/v42-hard-fork.md`). Upstream is never merged. Every so
often someone reads what Zed changed, ports the fixes a note-taker would miss, and records how far
the reading got. This skill is that procedure. `thock/script/upstream-candidates` does the
mechanical half; the judgment is yours.

## What git records

Nothing else tracks upstream: no ledger file, only trailers that `git log` can find.

| Trailer | On | Meaning |
|---|---|---|
| `Upstream-Reviewed-Through: <sha>` | the empty review commit that ends every review | every upstream commit up to and including `<sha>` was considered: ported or deliberately left out |
| `Upstream-Ref: <ref>` | the same commit | the ref that was reviewed (`v1.24.2`, or `main` for an urgent pick) |
| `(cherry picked from commit <sha>)` | a ported commit, written by `git cherry-pick -x` | cherry-picked from upstream |
| `Upstream-Commit: <sha>` | a hand-ported commit | ported by hand because the cherry-pick no longer applied; repeat the trailer when several upstream commits were folded into one |

PRs land as merge commits, so the trailers stay on `main`. If a review PR is ever squashed, put the
trailers in the squash message.

## Guardrails

- **Never push to `upstream`, and never open an issue, PR or comment on `zed-industries/zed`.**
  `gh` against upstream is read-only: `pr view`, `api` GETs.
- **Never fetch upstream tags into `refs/tags`.** Zed's `v*` tags are Thock's release trigger. The
  helper keeps the remote fetch-only with tags under `refs/upstream-tags/`; if it refuses to run
  because `upstream` has a push URL, fix the remote, don't work around the check.
- **A port never touches `crates/thock` or `thock/`.** When an upstream fix needs Thock-side
  changes, report it and let Diego decide.
- Don't resolve conflicts by pulling in neighbouring upstream changes. A port is the smallest diff
  that reaches upstream's end state.

## When to run

- Zed published a stable release (a `v*` tag without `-pre`). Compare against it, not `main`.
- A Zed security advisory touches a crate Thock still compiles. Run right away, with `--ref main`
  if the fix isn't in a stable tag yet.
- Diego asks.

## Procedure

### 1. Run the helper

```
thock/script/upstream-candidates > "$SCRATCH/candidates.tsv"
```

It repairs the `upstream` remote, fetches, finds the baseline (the newest
`Upstream-Reviewed-Through` on `origin/main`), picks the newest stable tag, drops commits Thock
already carries (listing them on stderr), and prints one tab-separated row per **live** commit:

```
sha  pr  subject  crates  release-note  signal  labels
```

`crates` lists the crates (or top-level directories) the commit touches that still exist in Thock.
A commit whose every path is in a deleted crate is **gone** and only counted. `signal` is a coarse
keyword hint: `security`, `crash`, `data`, `platform`, `perf` or blank. Options: `--ref <ref>` for
another target (`main`, a tag, a sha), `--base <sha>` to override the baseline, `--labels` to add
each PR's GitHub labels (one `gh` call per commit, so only on a short list), `--no-fetch`.

Report the range and the counts from stderr (`N commits, P already ported, L live, G gone`), then
check Zed's published advisories:

```
gh api repos/zed-industries/zed/security-advisories \
  --jq '.[] | "\(.published_at)\t\(.severity)\t\(.ghsa_id)\t\(.summary)\t\(.html_url)"'
```

Any advisory whose fix touches a live crate goes to the top of the list, whatever the range.

### 2. Triage the live commits into three tiers

Read the subject and the release note first; open the diff (`git show <sha>`) or
`gh pr view <n> --repo zed-industries/zed` only when they aren't enough. The `crates` column tells
you fast whether a commit can matter.

- **Port:** security fixes; crashes and panics; data loss or corruption in buffers, `fs` or saving;
  gpui platform fixes for macOS, Wayland or X11; vim correctness fixes.
- **Consider:** editor, vim, markdown, terminal and gpui behaviour or performance improvements a
  note-taker would notice, and `ui` component fixes.
- **Skip:** anything whose only live paths serve features Thock removed or never uses (LSP
  features, language-specific behaviour, code navigation, git UI, AI, collaboration); new upstream
  features; CI and tooling; Windows.

### 3. Check prerequisites

For each Port or Consider commit, note whether it depends on earlier upstream commits Thock doesn't
have: a new API, a moved file, a renamed type. Add the prerequisite to the candidate list, or
downgrade the commit to Skip with the reason.

### 4. Trial-apply

In a scratch worktree off `origin/main`, cherry-pick the Port and Consider commits in upstream
order and check each touched crate. Share the main checkout's target directory so the checks are
warm.

```
git worktree add --detach "$SCRATCH/trial" origin/main
cd "$SCRATCH/trial"
export CARGO_TARGET_DIR="$(git -C "$OLDPWD" rev-parse --show-toplevel)/target"
git cherry-pick -x <sha>        # on conflict: note it, `git cherry-pick --abort`, move on
cargo check -p <crate> ...      # one -p per crate the commit touched; never unscoped
```

Record for each commit: applied cleanly, conflicted, or applied but failed to build. Don't resolve
conflicts at this stage. Remove the worktree when done (`git worktree remove --force`).

### 5. Report and ask

Write the report to the session scratchpad: Port, Consider and notable Skips, each with its PR
link (`https://github.com/zed-industries/zed/pull/<n>`), live crates, trial result and a one-line
reason. Then ask Diego which commits to port with a multi-select question: the Port tier first and
marked recommended, the Consider tier after. Don't ask about Skips; they are in the report if he
wants to argue. More than a handful of candidates means several questions, not a longer one.

### 6. Port

On a branch named `upstream-<target>` (for example `upstream-v1.24.2`) off `origin/main`,
cherry-pick the chosen commits with `-x`, in upstream order.

- **A pick conflicts:** read the upstream diff for intent, resolve to the same end state with the
  smallest change, then `git add` the files and `GIT_EDITOR=true git cherry-pick --continue` so the
  upstream message and the `-x` line survive verbatim.
- **A pick can't be applied at all:** hand-port it as its own commit with the upstream subject and
  an `Upstream-Commit: <sha>` trailer.

Validate with `cargo check -p <crate>` and `cargo test -p <crate>` for every touched crate, then
`cargo fmt --all -- --check`, the Thock clippy line from `CLAUDE.md`, and `thock/script/test`.

### 7. Close the review

The branch's last commit is an empty one carrying the trailers. It is added even when nothing was
ported, so the next run starts after this range.

```
git commit --allow-empty -F - <<EOF
thock: Review upstream Zed through <target>

Upstream-Reviewed-Through: <full sha of <target>>
Upstream-Ref: <target>
EOF
```

### 8. Open the PR on DiegoTavares/thock

Title: `thock: Port upstream Zed fixes from <target>`. Body, in this order:

- **Ported** — one line per commit with its upstream PR link.
- **Considered and left out** — one line per Consider commit that wasn't taken, with the reason.
- **Release Notes:** — `- Fixed …` per user-visible port, or `- N/A` when nothing was ported or
  nothing shows.

Verify the remote before pushing (`git remote -v`): the branch goes to `origin`, never `upstream`.

## Final report to Diego

The PR URL, the range reviewed, the counts, what was ported and what was left out, which checks
ran, and anything that needs a Thock-side change.
