# Thock V41 - Report a problem from the phone, and the settings sheet it lives in

**Status:** Implemented (2026-10-07); ships with the next phone build and the next backend deploy
**Owner:** Diego · **Date:** 2026-10-07
**Companion docs:** `v33-iphone-companion.md` §2 (what the phone is and is not), `v34-vault-sync-api.md`
§6 (the device credential the route is authorised with), `v25-thock-plus-hosted-agent.md` (the Plus
service this adds a route to), `services/plus/README.md` (the API table and the deploy step)

---

## 1. Summary

The sheet behind the ellipsis on the phone was a diagnostics log: a paragraph of counters, a
raw address, the last error verbatim. V41 makes it the phone's one settings screen, with four
things on it, and gives the person a way to reach us that does not depend on a mail app:

1. **How this phone is doing with the desk**: a glyph in the state's colour, a headline and a
   sentence (*Up to date*, *7 waiting for the desk*, *Can't reach your desk*, *Thock Plus has
   ended*, *Disconnected by your desk*), when it last checked, and *Check again*.
2. **Report a problem**: a form with what happened, up to three screenshots, and the connection
   facts shown before they go. Sending files an issue on a private GitHub repository through the
   Plus service. The phone gets a number back and says thank you.
3. **Looks**: dark, light, or match the iPhone, unchanged.
4. **Disconnect this phone**, whose confirmation now says the desk keeps listing the phone until
   it is disconnected there as well, and that connecting again starts from the desk.

The practice notebook keeps its pretend-desk controls and has no report button: reporting needs a
phone that is connected, and the practice notebook is not.

## 2. Decisions

| # | Decision | Why |
|---|---|---|
| 1 | **No email.** The first cut opened a `mailto:` link; it is gone. | Half the phones have no mail app set up, the report's text was easy to lose on the way, and nothing arrived anywhere we could track. |
| 2 | **Reports go through the Plus service** (`POST /v1/vault/feedback`), authorised by the phone's device credential, phone role only. | The phone already talks to it, it already knows who the phone is, and the GitHub token stays on the server. A desk route can reuse the same handler later (DSK-08). |
| 3 | **A private repository** (`FEEDBACK_REPO`, by default `DiegoTavares/thock-feedback`), not the public tracker. | Screenshots show people's notes. Real bugs are moved to the public tracker by hand, without the picture. |
| 4 | **Screenshots are committed to that repository** under `reports/<date>/<time>-<phone>/<n>.png` and embedded in the issue by their `blob/…?raw=true` URL. | GitHub has no attachment API. A bucket would need public URLs; a file in a private repository renders for whoever may read the issue and for no one else. |
| 5 | **Connected phones only.** A phone whose Plus has lapsed may still report; the practice notebook and an unpaired phone may not. | A credential is a rate-limit key and a way to know which vault is affected. Without one the route would be an open form on a public service. |
| 6 | **Five reports per phone per hour**, in memory. | The service runs one instance (V25). Enough for a person, not for a script. |
| 7 | **Caps: 4000 characters, three images, 2 MB each, PNG or JPEG by their first bytes.** The phone downscales to 1600 pixels and sends JPEG. | A report is a few paragraphs and a picture or two. The server decides the type from the bytes, never from the declared one. |
| 8 | **The facts are a text block the phone writes** (`IssueReport.details`) and the server quotes. | The phone knows what it has: counts, versions, the last error, the files it could not take. The server adds what it knows: vault id, device id and name, the time. Neither sends a note. |
| 9 | **Status is a glyph, headline and sentence**, not a bar. | A bar implied progress where there was none: full and amber with seven changes waiting read as "done". The receipts feed already uses a dot per state; this is the same language. |

## 3. The route

`POST /v1/vault/feedback`, `Bearer <phone credential>`, `lapsedPhone: true`.

```json
{
  "description": "Captures stopped reaching my desk since this morning.",
  "details": "Thock for iPhone 1.0 (11) · iOS 26.0 · iPhone17,1\nConnection: connected as Diego's iPhone\nStatus: can't reach the desk's copy · 7 changes waiting for the desk\n…",
  "app_version": "1.0",
  "build": "11",
  "system": "iOS 26.0 · iPhone17,1",
  "screenshots": [{ "content_type": "image/jpeg", "data": "<base64>" }]
}
```

Answers: `201 {"number": 41}`; `400 bad_request` with a sentence the phone shows (empty
description, too long, too many or unreadable screenshots); `429 too_many_reports`;
`502 feedback_failed` when GitHub does not answer; `503 feedback_unavailable` when the service has
no repository or token. The role and credential refusals are V34 API §3's.

The issue: title `Phone: <first line, 72 characters>`, label `phone`, body with *What happened*
(the description as written), *Screenshots* (one image per line) and *Details* (a fenced block:
app and system line, the time, vault and phone ids, then the phone's details verbatim).

## 4. The form

`ReportProblemSheet`, presented over the settings sheet. A multi-line field (*What happened? What
did you expect instead?*), a row of thumbnails with a dashed *plus* that opens the photo picker
(up to three; each can be removed), and *Sent with it*: one sentence and a *Show the details*
link that prints the exact text going out. *Send* is enabled once there are words. On success:
haptic, *Sent. Thank you.*, both sheets close. On failure: an alert with the server's sentence.

## 5. Security

- The description and details are untrusted text that ends up in Markdown on GitHub; nothing on
  our side executes or renders them. Sizes are capped before anything is stored.
- Images are typed by their bytes and size-capped; a file that is not PNG or JPEG is refused.
- The GitHub token is a fine-grained token for the one repository with *Issues: write* and
  *Contents: write*, held in Secret Manager (`feedback-github-token`) and read by the service only.
- The phone never learns the repository; it gets a number.

## 6. Testing

- `services/plus/feedback_test.go`: a fake GitHub records what is filed. One report with a
  screenshot becomes one issue with the expected title, label, body and committed file; every
  validation refuses before anything is stored; the desk, an unauthenticated caller and a server
  without a tracker are refused with their codes; the sixth report in an hour is `429` and a
  GitHub outage is `502`.
- `ThockKit` `IssueReportTests`: the details text, the capped failure list, what the practice
  notebook and an unpaired phone say, the request payload (a fourth screenshot is dropped, types
  come from the bytes).
- The app target compiles and `thock/ios/script/smoke` passes on a simulator.

## 7. Setting it up

1. Create the private repository (`gh repo create DiegoTavares/thock-feedback --private`).
2. Mint a fine-grained token for it with *Issues: write* and *Contents: write*.
3. `FEEDBACK_GITHUB_TOKEN='github_pat_…' ./deploy.sh` from `services/plus`; the script stores the
   token and sets `FEEDBACK_REPO`. Until then the route answers `503` and the phone says reporting
   isn't set up yet.

## 8. Open items

- A desk-side *Report a problem* through the same route, with the desk credential (DSK-08).
- A reply path: the person never hears back inside the app. The invite email is the channel for
  now.
