# Design brief — engineer-cli · Review (spaced-repetition triage & the rate sitting)

**For:** Claude Design (the **engineer-cli** terminal project — not the web `engineer` project; see `../../README.md` for why they're kept apart).
**Produces:** the terminal Review screen and its three stages — the due dashboard, the rate sitting, and browse-all — anchored on `../../design-system.dc.html` (the live style anchor). The screen shipped straight from the daily-loop brief without a dedicated canvas doc; a `review.dc.html` board (mirroring `timer.dc.html`) is the natural home for a mock if the §8 residuals ever earn a design pass.
**Status:** **shipped.** The dashboard, the sitting, and browse-all are live (`src/app/screens/review.rs`, ~1272 lines; epic #7 daily-loop). This brief is kept as the module record; §4 is the shipped reality and §8 is the short list of residuals.

> **Module note.** This is one of the per-module briefs the terminal client decomposes into (see `../README.md`). It carries the Review pillar of the retired omnibus (`terminal-client.brief.md` job 8 — "review what's decaying") folded together with the daily-loop review job (see what's due, open the queue, rate each topic, advance automatically, exit cleanly mid-sitting). The shared house format — workflow → jobs → principles → orientation → the API it consumes → visual language → phasing — is common to every module brief.

---

## 1. Who this is for (the workflow)

> "Once a day I want to know what's decaying — not a wall of charts, just: how many topics are due, roughly how long they'll take, and then let me *rate them one at a time* without ceremony.
> Show me the topic, I press one key for how well I remembered — `f` forgot, `z` fuzzy, `s` solid, `i` instant — and the next due topic is already there. I don't confirm, I don't navigate; the queue drains under my fingers and when it's empty it says so.
> If I get pulled away mid-sitting I press `Esc` and I've lost nothing — every rating I already gave is committed.
> Browsing the whole catalogue is a thing I do occasionally — find a topic, read its prompts, maybe rate it off-cycle — but it's secondary to the daily drain."

The throughline: **the daily review loop is a triage list, not a dashboard.** The web `review.html` leads with a heatmap; the terminal deliberately does not (§7). What survives the translation to a character grid is the *due count as one honest line*, the *queue in urgency order*, and the *sitting as a single-keystroke drill* — the payoff the whole screen exists to deliver.

---

## 2. The jobs the design must do (outcomes, not mechanisms)

**8a. Read what's decaying — the due triage.** The dashboard: how many topics are due, an estimated-minutes read, the streak/cadence stats, and the due queue in urgency order — a scannable terminal triage list with quiet freshness badges, not a nag. *Shipped* (`render_dashboard`).

**8b. Rate each due topic — the sitting.** Open the queue head, rate it with a single keystroke (`f`/`z`/`s`/`i` = forgot/fuzzy/solid/instant), the server hands back the next due topic, advance automatically, and drain to a quiet "done" view. Each rating is committed per topic so `Esc` exits cleanly mid-sitting with nothing to confirm. *Shipped* (`render_sitting`, the `Rating` enum, the `ReviewRate`/`ReviewRated` loop).

**8c. Browse the whole catalogue — secondary.** The full topic list, paginated, with the API's sort ring on `s`, a server-side `q` search on `/`, and `↵` into a topic detail read (prompts + per-rating interval forecasts) with a one-off rate option. *Shipped* (`render_browse`, `render_detail`) — explicitly the secondary surface, entered with `b` and returning to the dashboard with `h`/`Esc`.

---

## 3. Principles that genuinely bind

- **Derived freshness, never a stored client ledger.** A topic's state, interval, due-ness, and per-rating forecasts are computed server-side on every read (ADR 0019 — the Review pillar is RPC-flavoured; a topic is keyed by its `subdomain_id` and rated through the model's `record_review!`). The client renders what it's handed and re-reads after every write; it keeps no second schedule. Rating is the only write this module owns.
- **Ratings are single keystrokes.** The four ratings map to `f`/`z`/`s`/`i` — one distinct, mnemonic letter each (forgot / fuZZy / solid / instant). The set deliberately avoids `r` (the global refresh key), which is also why the screen is opened with `R`, not `r`. No menu, no arrow-to-select: the whole sitting is one press per topic.
- **Modal grammar.** Dashboard and browse are the two base stages handled by the global keymap; the sitting, the browse detail read, and the browse search prompt are modal states that intercept keys *before* the global map (`intercept_key`), so `s` means "solid" in the sitting and "sort" in browse without collision. `Esc` always steps out one modal layer.
- **Quiet due-badges — triage, not nagging.** The due count is one calm line (`3 topics due · ~5 min`); the empty state is a success tick (`Nothing due — you're all caught up ✓`); freshness reads as muted `state · Nx · 21d`, never a red alarm. Reviewing is invited, never demanded — hold this in any new surface.

---

## 4. What's already in the app (orientation — shipped reality)

- **One screen, three stages** (`src/app/screens/review.rs`, `Stage::{Dashboard, Sitting, Browse}`):
  - **Dashboard** — the due count + `est_minutes`, the `stats_line` (`streak N days · best M · K this month · avg 21d interval`), and the due queue in urgency order with per-row freshness (`queue_line`). `↵`/`s` start the sitting, `b` browse, `r` refresh, `h` home.
  - **Sitting** — the queue head rendered with its domain, a context line, and its prompts; the four rating keys shown as black-on-accent caps with the interval each would set (`→21d`, from the payload's `forecasts`). A rating fires `rate_topic`, the response's `next_topic` becomes `current` (or the view flips to "Sitting complete ✓"), and a `rating_in_flight` guard blocks double-rating one topic. `Esc` (or `h`) exits to a refreshed dashboard.
  - **Browse** — a paginated `Table` (topic / domain / state / reviews / interval), the sort ring on `s` (`urgency → recent → most/least reviewed → longest interval → a–z`), `[`/`]` paging, `j`/`k` + `gg`/`G` movement, `/` for a server-side `q` search, and `↵`/`l` into a detail read that opens instantly from the row then refines with the full record (prompts + forecasts the list omits). Rating from the detail closes it and refetches the page so freshness mirrors the server.
- **The read/write client is wired and tested** (`src/api/review.rs`): `review_dashboard` (`GET …/dashboard`), `list_topics` (`GET …/topics` with the `TopicFilters` query), `get_topic` (`GET …/topics/:subdomain_id`), and `rate_topic` (`POST …/topics/:subdomain_id/rate`). The `Topic`, `Dashboard`, `ReviewStats`, and `RateResult` shapes are all consumed by the screen.
- **Dead client code, no face:** `list_review_sessions` (`GET …/sessions`) is implemented and compiles under the module's `#![allow(dead_code)]`, but nothing routes to it — there is no sessions-history surface yet (§8).
- **Deliberately parsed-but-not-rendered:** `Dashboard.heatmap` (and its `Heatmap`/`HeatCell` types) is deserialized by the API layer and then *not* drawn. The module doc-comment states this as an intentional non-goal, not an omission (§7).
- **No headless twin.** Unlike the timer (`src/timer_cli.rs`) and the pace read, Review has no `engineer review` one-shot — the daily loop is inherently interactive (rate → advance), so the TUI is the whole surface. This was a scoping call, not a gap to close reflexively (§8).

---

## 5. The API it consumes (verified against `engineer/config/routes.rb`)

The Review endpoints live under `namespace :review` (`routes.rb:395-401`), all shipped server-side. Freshness is derived on read, so the surface is RPC-flavoured (ADR 0019): a topic is addressed by its `subdomain_id` and mutated only by rating, never by writing a schedule row from the client.

- **Dashboard read** — `GET /api/v1/review/dashboard` (`get :dashboard, to: "dashboard#show"`). One object: `stats`, `est_minutes`, the `heatmap` (parsed, not rendered), and the due `queue` (topics in urgency order). Consumed by `review_dashboard`.
- **Topics** — `resources :topics, only: %i[index show], param: :subdomain_id`:
  - `GET /api/v1/review/topics` — the browse list, one row per topic; honours `domain_id`, `state`, `q`, `sort`, and `page` query params (`TopicFilters`). Paged via the response `meta` (`page`/`per_page`/`total`). Consumed by `list_topics`.
  - `GET /api/v1/review/topics/:subdomain_id` — the full topic (adds the prompts/notes and per-rating `forecasts` the list omits). Consumed by `get_topic`. The path param is `subdomain_id`, not a synthetic topic id — the topic *is* its subdomain.
  - `POST /api/v1/review/topics/:subdomain_id/rate` (`member { post :rate }`) — the one write. Body `{ "rating": "forgot|fuzzy|solid|instant" }`; returns `{ topic, next_topic }` (the rated topic re-read + the next due one, which drives the sitting's auto-advance). Consumed by `rate_topic`.
- **Sessions** — `resources :sessions, only: %i[index]` → `GET /api/v1/review/sessions`, a reviews-history read. The client method (`list_review_sessions`, returning `ReviewSession` rows) exists but is unrouted — this is the residual in §8.

---

## 6. Visual language (hard constraint — do not drift)

Bind to this repo's kit: `../../README.md` (chrome + palette mapping + translate/don't-translate) and `../../design-system.dc.html` (the live style anchor — the retired `books.html`/`terminal-tokens.css` the omnibus cited no longer exist; never reference them). Assemble from shipped atoms only: `bordered()` panels, full-row inverse selection with `▌`, the black-on-accent key caps the rating hints use, `footer_hints` per stage, `notify` tiles for rate results and errors, muted freshness reads. Keyboard-only, neovim-flavoured; the footer advertises the active stage's keys. ASCII-only — status dots and unicode glyphs sparingly (`✓` for the caught-up/complete states, `·` separators). The pending accent-hue decision (periwinkle `105` vs shipped sky-blue `75`) is one the cross-cutting brief tracks — it touches the rating caps and selection here, but is not this module's to resolve.

---

## 7. Out of scope

**The heatmap is a decided non-goal, not a backlog item.** The web dashboard's activity heatmap is a bar/grid chart that does not reduce to a single scannable line in a character grid — the streak and this-month counts already convey cadence in one muted line, and rendering a heat grid would fight the "triage, not dashboard" throughline. The payload is parsed so the read stays lossless, but the render is deliberately omitted (see the module doc-comment). **A future pass must not "add" it back by mistake — reinforced in §8.**

Also out: any write beyond a rating (there is no client-side reschedule, snooze, or edit-interval — freshness is server-derived); a stored client-side review ledger; light mode; and a headless review verb (the loop is interactive by nature — see §4/§8).

## 8. Phasing — residuals only (this module is shipped)

1. **Sessions history has no face.** `list_review_sessions` / `GET /api/v1/review/sessions` is dead client code today (implemented, tested-shaped by `ReviewSession`, but nothing routes to it). The natural surface is a "recent reviews" log — a browse subtab or a dashboard drill showing what was rated and when (`reviewed_at`, `rating`, topic). Small, additive, unblocked on the server. Grounds on the already-shipped `sessions` route in §5.
2. **The heatmap stays a non-goal — do not add it.** This is a residual *note*, not a residual *task*: a future contributor scanning `Dashboard.heatmap` for an unrendered field must read §7 first. If richer cadence is ever wanted, the honest terminal move is a one-line sparkline reduction (as the timer/progress rails do from a day series), *not* the web's grid — and that would be a design decision to raise, not a silent addition.
3. **A headless due-count, only if a real need appears.** There is deliberately no `engineer review` one-shot. If a status-bar "N due" reduction is ever wanted it would reuse the dashboard read, bound by the quiet-by-default rule — but it is not a stated job, so it is listed here as a possibility, not a gap.
