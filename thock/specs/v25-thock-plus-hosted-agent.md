# Thock V25 — Thock Plus Subscription & Hosted Agent

**Status:** Decisions locked from research review (2026-09-09), pre-implementation
**Owner:** Diego · **Date:** 2026-09-09
**Companion docs:** `../VISION.md` (§4.3 "Bring your own brain" — amended by this spec, §9 trust model, §12 roadmap), `v5-agent-and-onboarding.md` (BYO rails this builds beside — and whose "no ACP client work" non-goal this supersedes), research brief: https://claude.ai/code/artifact/b6ac65e0-8012-4a51-aac2-f920a4598b7d

---

## 1. Summary

V25 adds a paid tier — working name **Thock Plus** — and its first benefit: a **hosted Thock Agent**
that works out of the box for people who will never install a CLI or paste an API key. The tier is not
"the agent tier": it is the home for **everything that costs Thock money to serve** — hosted agent
now, sync storage and similar services later. BYO-agent stays free and first-class forever; that
promise is written into the VISION amendment this spec carries.

The shape, locked after the 2026-09 research pass:

- **Seam:** ACP. The fork already ships the full client stack (`acp_thread`, `agent_servers`,
  registry auto-install via managed Node); the harness is a swappable subprocess.
- **Surface:** a **Thock-owned chat panel** built on `acp_thread`/`agent_servers` *as libraries* —
  friendly chat, not a code harness. Zed's `agent_ui` stays hidden.
- **Harness:** **Pi** (MIT, earendil-works), launched over ACP via the `pi-acp` adapter,
  auto-installed through the existing registry machinery. Chosen 2026-09-11, superseding the
  2026-09-09 OpenCode pick: ~5–7× lower fixed token overhead (~1.0–1.4k vs ~6.9k tokens/request —
  vendor-paid margin), native OpenRouter support with env-var key injection, offline model catalog,
  and a fully swappable system prompt (a note-taking prompt instead of a coding one).
- **Models:** cheap/fast by default (candidate: Gemini Flash) through **OpenRouter**; users see
  abstract tiers (Default/Fast), never model names or keys.
- **Trust model:** **no per-change approval prompts** — real use showed them to be friction, not
  trust (decision 16, and the matching VISION §4.4 amendment). The agent acts freely inside the
  vault; safety is scope + undo: OS sandbox, append-don't-rewrite, checkpoint before every session.
- **Money flow:** Polar subscription + Credits benefit (hard stop, no metered overage) + one-time
  top-up packs. A small Thock backend validates the Polar license key, mints one budget-capped
  OpenRouter provisioned key per user, ingests usage events back to Polar's meter.
- **Safety:** budgets, not DRM — short-lived/revocable credentials, hard per-user spend caps, and a
  Thock-enforced OS sandbox on the harness process (write = vault only, network = gateway only).

Delivery is **two stages, Polar last**. Stage 1 is the whole agent experience — ACP + OpenCode +
OpenRouter + the chat panel — running against backend-issued dev entitlements, so the
chat/server/allowance loop can be iterated freely before any billing exists. Stage 2 bolts money and
safety on: Polar products, license keys, credits ingestion, sandbox hardening. To make that order
(and future price changes) structural, the backend owns **plans and allowances as configuration** —
Polar is a driver that grants allowance and issues credentials, never the source of truth.

## 2. Locked decisions (2026-09-09)

| # | Decision | Choice |
|---|---|---|
| 1 | Subscription scope | **Thock Plus gates all cost-to-serve features**, not just the agent. Hosted agent is benefit #1; sync storage etc. arrive under the same tier in later specs. One Polar subscription product, benefits added over time. |
| 2 | BYO posture | **Free and first-class forever.** The v5 terminal rails remain the free path, untouched. VISION gets the sibling promise: *"your agent, or ours — never a lock-in."* |
| 3 | Chat surface | **New Thock-owned panel** in `crates/thock`, consuming `acp_thread` + `agent_servers` as libraries. Friendlier chat look; streams the agent's activity (files touched, diffs) in plain language as it works. Do **not** unhide or restyle Zed's `agent_ui`. |
| 4 | Default harness | **Pi** via ACP (`pi-acp` adapter; MIT). Decided 2026-09-11, superseding the 09-09 OpenCode pick — token economics (~5–7× lower fixed overhead on vendor-paid tokens), native OpenRouter + env-var key injection, offline catalog, swappable system prompt. Pi's lack of a built-in permission system is a **fit**, not a gap, given decision 16. OpenCode demoted to fallback; Claude Agent SDK remains the premium hedge. The ACP seam keeps all three swappable by config. |
| 5 | Default model | **Cheap/fast first** — candidate Gemini Flash via OpenRouter slug. Tiers stay abstract (`Default`/`Fast`, as in `agent.rs` today); mapping lives in Thock-controlled config, not user-visible model pickers. |
| 6 | Gateway | **OpenRouter provisioned keys first** (zero ops, per-user dollar caps, auto-reset). Migrate to self-hosted LiteLLM when volume justifies TTL keys, rate limits, and fee recovery. Migration is backend-only; clients never notice. |
| 7 | Billing | **Polar**: fixed monthly price + Credits benefit granting N units/cycle on a usage meter, **no metered price** → hard stop, no surprise bills. One-time **top-up** products for heavy months. No rollover. |
| 8 | Meter unit | **Normalized units (cost-in-cents based)**, not raw tokens — allowance survives model switches. |
| 9 | App credential | **Polar license key benefit** is the app's credential to the Thock backend (validate endpoint needs no auth; auto-revokes on cancel). Backend maps license key → external customer ID → OpenRouter key. |
| 10 | Enforcement | **Gateway-side hard stop** (Polar never blocks usage). Backend caches Customer State, counts down locally, warns at 80% in-app, refuses at zero with a top-up prompt. |
| 11 | Sandbox | **Thock wraps the harness process** with OS-level confinement (Seatbelt/bubblewrap — in-tree `crates/sandbox` or Anthropic's `sandbox-runtime`): write = vault dir, network = gateway only. Pi ships no sandbox of its own; this is non-negotiable for the hosted path — with no approval gates it is also the primary prompt-injection defense. |
| 12 | Abuse posture | Budgets not DRM: revocable per-user keys, hard caps, kill on refund/chargeback. No client attestation. |
| 13 | VISION | Amendment in the Phase 0 PR (and republish of the VISION artifact at its existing URL): BYO stays first-class; §10's "never the intelligence" becomes "intelligence optional, never required". |
| 14 | Plans are config | **Pricing, allowances, model-tier mapping, and per-plan limits live in backend configuration** (hot-changeable, versioned), never hardcoded in app or backend code. Polar products reference plans; they don't define them. |
| 15 | Delivery order | **Two stages, Polar last.** Stage 1 = ACP + Pi + OpenRouter + chat panel on dev entitlements — full iteration loop with zero billing setup. Stage 2 = Polar (checkout, license keys, credits) + safety hardening (sandbox, ceilings, revocation). |
| 16 | Trust model | **No per-change approval prompts** (decided 2026-09-11 — the original confirmation-gate assumption was dropped after real use showed it to be friction). The agent acts freely inside the vault. Safety = OS sandbox scope + append-don't-rewrite convention + checkpoint-before-session (invisible history) + hard budget caps. Irreversible or outside-the-vault actions stay human-confirmed by ritual design (money, email), not by app-level gates. VISION §4.4 amended to match. |

## 3. Goals & success criteria

**Primary:** a non-technical user (the wife test) buys Thock Plus in a browser, pastes nothing,
installs nothing, and asks the Thock Agent to run "Wrap Today" from a chat panel — no approval
popups to click through, activity they can read, a usage bar they can understand, and a bill that
can never exceed the sticker price.

**Definition of done — Stage 1 (agent rails: ACP + harness + OpenRouter, no billing):**
1. A backend service (small: Fly/Railway-class) exists with a **billing-agnostic core**: users +
   entitlements in its own store, **plans defined in configuration** (allowance in normalized units,
   model-tier mapping, per-plan limits — hot-changeable without deploys), OpenRouter provisioned-key
   mint/revoke sized from the plan config, and its own usage ledger (normalized units per user).
2. **Dev entitlements**: the backend can grant an invite-code/dev user a plan without any payment
   system — this is how Diego, the wife test, and early testers run for all of Stage 1.
3. "Connect Thock Agent" in the app: enter an invite code, fetch the gateway credential,
   auto-install Pi via the registry machinery, inject the OpenRouter provider config + per-user key
   through the launch environment (`--provider openrouter --model <tier-mapped id>`, key via env) —
   zero manual setup, no key pasting.
4. A new right-dock **Thock Agent chat panel** (`crates/thock`, unique `activation_priority`, full
   keyboard navigation per the panel rules) hosts ACP sessions with Pi (via the `pi-acp` adapter):
   message list, streaming responses, tool activity in plain language, diff previews. It reads as a
   friendly chat, not a code harness.
5. **Trust without gates**: no per-change approval prompts anywhere (decision 16). Instead, every
   hosted session requests a pre-session checkpoint from the invisible-history service when it
   exists (soft dependency, v5-style — launch anyway if it isn't shipped), the panel shows what the
   agent touched as it works, and a Stage 1 gate is validating the `pi-acp` adapter end to end
   (session lifecycle, streaming, tool-call status, cancellation) — it is a third-party MVP and
   Thock must be ready to fork it if it rots.
6. Skills and Routines run through the same session; the kickoff prompt convention ("Read and
   execute `<vault-relative path>`") is unchanged, so shipped skills work on both paths.
7. Allowance mechanics work end to end without Polar: usage counts down against the config-defined
   allowance, the panel footer shows the balance, 80% warns, zero hard-stops, and the backend can
   reset/adjust any user's allowance by config or admin action (the iteration loop).
8. End-to-end smoke test: hosted session runs a skill against Gemini Flash through OpenRouter;
   usage lands in the backend ledger; exhaustion refuses further requests; revoking a dev
   entitlement revokes the key and the app falls back cleanly to the free BYO path.
9. The BYO terminal panel remains available and unchanged; connection mode (BYO / Thock Agent) is an
   explicit, switchable choice.

**Definition of done — Stage 2 (money + safety: Polar on top of the working core):**
10. Polar products exist: Thock Plus subscription (fixed price + Credits benefit + license key
    benefit) and a top-up product, each **referencing a backend plan by id** — prices and allowances
    keep living in backend config. A "Get Thock Plus" entry opens the checkout link in the browser.
11. The backend gains a Polar driver: license-key validation as the app credential (replacing invite
    codes for paying users; dev entitlements remain for testing), entitlement from Polar Customer
    State (cached, webhook-refreshed), usage-event ingestion to Polar's meter alongside the local
    ledger, lapse/refund/chargeback webhooks revoking keys immediately.
12. "Manage subscription" opens the Polar customer portal; checkout deep-links back into the connect
    flow.
13. OS sandbox policy enforced on every hosted session (write = vault, network = gateway + pinned
    catalog allowlist); per-session turn/token ceilings from plan config.
14. Anomaly guardrails: velocity alerts on the backend ledger; one plan per payment identity.

**Beyond v25 (not gated):** migrate the gateway to self-hosted LiteLLM (24h virtual keys, TPM/RPM
caps, model allowlist, fee recovery — client config unchanged); optionally wire the Claude Agent SDK
as a premium hedge harness behind the same gateway.

## 4. Non-goals

- **No changes to the free BYO experience.** The v5 terminal rails are untouched; the chat panel is
  hosted-tier-only until a future spec says otherwise.
- **No server-side agent execution.** Notes never leave the machine for the agent's sake; only model
  traffic (prompts/completions) transits the gateway. No vault sync in this spec — that is a future
  Thock Plus benefit with its own spec.
- **No model picker, key fields, or provider names in the UI.** Tiers only.
- **No per-change approval prompts.** Dropped by design (decision 16). Rituals may still ask
  questions as part of their own flow (triage confirms filing, the money ritual waits for the human)
  — but the app imposes no write gates on the agent.
- **No metered overage billing.** Hard stop + prepaid top-ups only; nobody gets a surprise invoice.
- **No client attestation / DRM.** Extractable credentials are assumed; budgets bound the damage.
- **No multi-seat/team plans** in v1.

## 5. Architecture

```
User's machine                        Thock cloud (small)              Third parties
┌─────────────────────────────┐      ┌──────────────────────────┐     ┌─────────────────────┐
│ Thock Agent chat panel      │      │ Backend                  │     │ OpenRouter          │
│   (acp_thread as library)   │      │  license-key auth        │     │  per-user keys,     │
│         │ ACP over stdio    │      │  entitlement cache       │     │  $ caps, routing    │
│ Pi subprocess (pi-acp)      │◄────►│  key mint/revoke         │────►│    │                │
│  env: OpenRouter key,       │HTTPS │  usage → Polar meter     │     │  Gemini / Anthropic │
│  provider cfg, tier map     │      │  webhook receiver        │     │  / OpenAI upstreams │
│ OS sandbox: write=vault,    │      └──────────────────────────┘     ├─────────────────────┤
│  net=gateway allowlist      │                                       │ Polar.sh            │
│ Vault (plain Markdown)      │                                       │  checkout, credits  │
└─────────────────────────────┘                                       │  meter, license keys│
                                                                      └─────────────────────┘
```

Request flow (target, Stage 2): checkout (browser) → Polar grants license key + cycle credits → app
validates key with backend → backend mints capped OpenRouter key → panel spawns sandboxed Pi
with key/config in env → Pi ↔ OpenRouter ↔ model → backend ledger + Polar meter ingest →
80% warn, 0 = stop, top-up or wait for cycle.

In Stage 1 the same flow runs with the Polar column absent: invite code instead of license key,
plan config + backend ledger as the only allowance authority. Nothing in the app or the gateway
path changes when Polar arrives — Stage 2 swaps the credential and adds ingestion.

## 6. Known risks & retests

- **`pi-acp` is nobody's product**: a one-person MVP adapter (last published 2026-07-30, pinned to
  pi ≥0.80.4 while core ships weekly at 0.85.x); official ACP support in Pi core is an unanswered
  discussion. Without permission bridging the surface Thock depends on is small (session lifecycle,
  streaming, tool status), but validate it end to end in Stage 1 and budget for owning a fork.
- **Pi 0.x velocity under Earendil**: corporate backing is a stability plus, but the roadmap serves
  in-process SDK embedding (OpenClaw), not ACP subprocess embedders; expect breaking changes.
- **First-launch downloads**: Pi fetches `fd`/`rg` helper binaries on first run — pre-bundle them or
  allow the download before the sandbox tightens; the model catalog itself ships built-in and caches
  offline (no models.dev-style hard dependency).
- **No-gates trust model leans on the checkpoint service** (VISION M0, still in progress): until
  checkpoint-before-session lands, an errant session is only as recoverable as the last snapshot.
  Prioritize it alongside Stage 1; append-don't-rewrite remains convention, not enforcement.
- **Cheap-model behavior unproven head-to-head**: no published Pi-vs-alternatives edit-failure data
  on Gemini Flash. Run a small internal eval (~20 vault-shaped tasks) during Stage 1; the ACP seam
  keeps OpenCode and the Claude Agent SDK one config swap away.
- **OpenRouter key granularity**: provisioned keys cap dollars but not RPM/TPM, and model
  restriction per key is unverified — Stage 1 relies on spend caps alone; LiteLLM closes this.
- **Margin variance**: heavy users ride the full allowance; the hard cap bounds it, but tier pricing
  must assume worst-case allowance burn (see open items).
- **Prompt injection in vault files**: with no approval gates, the sandbox network allowlist is THE
  exfiltration defense — never widen it casually, and land the sandbox before any user who isn't
  Diego runs hosted sessions.

## 7. Open items

1. **Initial plan config values**: allowance (normalized units), model-tier mapping, per-plan
   limits — needed for Stage 1, but freely tunable since plans are config (decision 14). The public
   monthly price and top-up size can wait until Stage 2. Fee tier resolved: the org dates from
   Feb 2026 → **legacy 4% + 40¢ applies**. Do not upgrade to the newer Polar plans without
   re-modeling — upgrading forfeits the legacy rate permanently.
2. **Tier naming**: "Thock Plus" is a working name.
3. **Panel design pass**: what "looks like a chat, not a code harness" means concretely (bubbles vs
   blocks, tool-call rendering, empty state).
4. ~~VISION amendment text~~ — done 2026-09-11: §4.3 (Thock Plus, BYO first-class), §4.4 (trust =
   scope + undo, gates dropped), §6/§9/§10 annotations, Milestone 5 added; artifact republished at
   its existing URL. Review the diff before committing.
