# Capability Core — Situation Report and Handover

| Field | Value |
| --- | --- |
| Repository | `https://github.com/Stark-work-1001/capability-core` |
| Version examined | 1.0.0 |
| Date | 2026-10-20 |
| What this is | Context, evidence, and an open brief. Not a task list. |
| Who it is for | A stronger model than the one that wrote it. |

---

## 0. What I am handing you, and what I am not

This document exists because I looked at this codebase and found things you should know before you start. It is not a specification. It is not a checklist. **Nothing in it is binding.**

What it contains:

- **Evidence.** Things I verified by reading the published source and, for the security and contract items, by running the published code against a throwaway database. Each carries a reproduction command. I expect some of these to be wrong, and I expect you to find others that are worse.
- **An assessment** of where the structural weight sits, written as reasoning you can argue with.
- **Some instincts** about what matters. Opinions with reasons attached, not requirements.

What it deliberately does not contain:

- **A fix for anything.** What to do is your call. Where I sketch a direction it is one option among many, and I have not costed it.
- **An order of work you must follow.** I suggest one in §5. If you see a better sequence, use yours.
- **Targets you must hit.** Where I name a number it describes a failure mode, not a budget I am imposing.
- **The complete list of what is wrong.** I found what one pass finds. The real list is longer, and you will write most of it.

One sentence that matters more than the rest:

> **If the best thing to do here is not on this list, do that instead, and explain why. I would rather be corrected than obeyed.**

A second sentence, about authority:

> **This project is handed to the open-source community. Whatever follows is a starting point, not a mandate. Fork it, re-architect it, or argue that half of it should be deleted — all are legitimate outcomes. The judgement is yours, and I have no standing to overrule it.**

That includes the repository's own contents. The `old/` directory is a historical snapshot, and when the live code has moved past it, deleting it is a legitimate maintenance decision, not an act of vandalism. See §2.9.

---

## 1. The situation

Capability Core is a local, cross-host capability hub for AI coding agents. It reaches into several hosts on the same machine, extracts skills, MCP server declarations, plugins, prompts and agent cards, normalises them into one SQLite model, and serves that model over MCP and REST. On top of retrieval it diagnoses faults, arbitrates duplicates, compares upstream versions, and repairs — either by applying a small set of safe fixes itself, or by writing a work order an attached agent can pick up. It additionally holds its own capability assets on disk, and can call cloud models for embedding, reranking and judgement while falling back to local algorithms when they are unavailable.

It is a one-person project at version 1.0.0. It works. It demos well. It has a real corpus behind it: **310 capabilities, 316 installations, 22 sources, and 35 passing tests**.

The honest shape of it, as I read it:

**The product's thesis is good and badly underexploited.** "The hub diagnoses and locates; an agent does the repair" is a genuinely interesting division of labour. But the repair path currently ends at a file write with no verification, no ownership, and no proof that anything improved. **The most interesting idea in the codebase is the least finished part of it.** That is where I think the value is.

**The breadth is real and the depth is thin.** Six protocol adapters, three advertised retrieval channels, a nine-rule diagnostic engine, a duplicate arbiter, an update checker, an ecosystem sync, a model-integration layer with four decoupled roles, and a self-owned asset library. Every one has a correct skeleton. Several are shallow. **One retrieval channel has still never held a single row** — see §2.2.

**The architecture is one process doing four jobs.** The Electron main process is simultaneously the kernel, the HTTP server, the MCP server and the scheduler. The database driver is synchronous by design. A scan therefore blocks the window, and every service is testable only by booting Electron. This survives at this size and will not at the next order of magnitude.

**The code is honest about its own uncertainty, and that is worth preserving.** Degradation is surfaced rather than hidden. Backups exist. Deletion is reversible. There is an audit trail. These are much harder to add later than to keep. A rewrite that loses them has lost more than it gained.

What this codebase needs is not more features and not a rewrite. It needs **the parts that already exist to become trustworthy, and the one idea that is unique to it to become real.**

---

## 2. What I verified

Everything here was reproduced against the published code. Appendix A gives the command for each. **Treat these as claims to check, not facts to accept.** If one does not reproduce, that tells you something about me and something about the code, and both are useful.

I have grouped them by what they suggest about the system rather than by severity, because the grouping is the informative part.

### 2.1 The trust boundary is weaker than its description

**A repair ticket can write outside its own installation. Still.** `submitPatch` computes its permitted prefix from `path.dirname()` of the installation path — one level up — so a fault raised for one skill permits writing any sibling skill under the same parent. I reproduced this against the current code: a `skill-missing-frontmatter` fault on `victim-a` overwrote `victim-b`'s entry file and returned success.

```js
const allowedRoot = inst ? path.resolve(String(inst.path).split('#')[0]) : null;
if (!allowedRoot || (target !== allowedRoot && !target.startsWith(path.dirname(allowedRoot) + path.sep))) {
  throw new Error('补丁路径越界：' + target);
}
```

The intent is right and the enforcement is a neighbour of the intent. For MCP installations the degradation is worse: the stored path carries a `#serverName` suffix, so the permitted prefix can collapse to the whole configuration directory. This function has **no tests at all**, which is likely why it has survived two rounds.

**The HTTP surface has no authentication and answers any origin.** `access-control-allow-origin: *` is set unconditionally in `server/http.js` (lines 190 and 198), `OPTIONS` returns 204 for everything, and no write endpoint checks a credential. `POST /capabilities/import` will copy an arbitrary directory into the managed library; `/ecosystem/apply` writes into other applications' config files; `/faults/:id/autofix` rewrites skill files on disk. From a browser on the same machine these are reachable.

**Volume and origin are enforced only where they were thought of.** The write endpoints accept bodies up to a configured cap, and path traversal is guarded in some call sites — see §2.7 — but the guard is per-call-site, not one chokepoint. A reviewer must reason rather than grep.

The pattern across all three: **the intent is documented correctly and the enforcement is one level weaker.** That is a specific kind of bug — it means the author knew the rule and implemented a neighbour of it. It also means the fix is usually not "add a check" but **"make the check structurally impossible to weaken."**

### 2.2 Advertised capability that does not exist

**The trigram retrieval channel has never held a row. Confirmed on the current code.**

```
search_docs    = 310
search_fts     = 310
search_trigram = 0
```

The table is created as a contentless FTS5 index with `tokenize='trigram'` (`db/schema.js:135`). Exactly two files mention it: the schema that creates it and the query in `search.js:82`. **Nothing inserts into it** — no trigger, no rebuild path, no maintenance code. The query sits inside a `try/catch` that swallows the failure, so the loss is completely silent.

This was the channel meant to make Chinese substring and path-fragment queries work. The README describes it as a working part of a three-way fusion. It contributes nothing, and nobody could have noticed from outside.

**A settings toggle is written and never read.** `diagnoseOnScan` appears exactly once in the entire source tree: `renderer/views/SettingsView.tsx:70`, the view that writes it. No main-process code reads it. An operator enables it, nothing happens, and they lose a little trust in every other switch on that page.

**A shell command is executed from stored configuration without validation.** MCP entries carry a `command`, and the fault analyser probes whether it resolves. That is fine for diagnosis. But the same stored value is what an attached agent is told to run, and nothing distinguishes a benign `npx` from anything else. If you make the hub dispatch work, this becomes an honour-system boundary.

These three share a cause: **capability declared at the surface, implemented partly at the core, and no test asserting the two agree.** Any structure you build should make that class of gap impossible to ship, not merely fix these instances.

### 2.3 Verification is a label, not a check

This is the one I would think hardest about.

```js
// services/faults.js:224-226 — after autoFix writes the file
UPDATE faults       SET status='resolved', ...
UPDATE capabilities SET health='healthy', verify_state='verified', ...
```

Neither `autoFix` nor `submitPatch` re-runs the rule that raised the fault. **A patch that changes nothing is recorded as a verified repair.**

Meanwhile the repair writes have no compare-and-swap. `repairs.before_hash` records what was read; nothing checks the file still matches at write time. Two concurrent patches both apply, the last writer wins, and the log claims two successes.

And the ticket has no owner. There is a dispatch action and a submit action with **nothing between them** — no claim, no lease, no state machine, no way to distinguish a worker that died from one that is slow.

For a product whose entire claim is "the hub can verify and govern repairs", these three together mean **the claim is unearned**. I do not think this is a bug to patch. I think it is the missing core of the product.

### 2.4 Things that will not scale

Measured at 310 capabilities on a current-generation machine. Not wrong at this size; wrong at the next.

| Observation | Measured | Growth |
| --- | --- | --- |
| `dedupe.scan` is an all-pairs loop that re-tokenises both sides per pair and re-reads open groups from SQLite per candidate | 433 ms | quadratic — ~40 s at 3,000, over an hour at 30,000 |
| Every scan calls `reindexSearch`, deleting and rebuilding all documents in one transaction | 240 ms | linear per scan, on a path where per-document maintenance already ran |
| `countForHost` full-scans the capability table once per agent | one scan per known host per `list()` | linear per host |
| `upsertCapability` issues ~5 statements per capability with no surrounding transaction | ~1,500 implicit transactions per scan | linear, and a crash leaves the library half-updated |
| Every statement is prepared on each call; the driver caches nothing | thousands of prepares per scan | linear |
| Every search writes an `events` row, and `events` has no retention | one row per query, forever | unbounded |
| `model_calls` grows the same way, with no retention | one row per model call, forever | unbounded |

A correctness problem hides inside the performance one: **the scan is not transactional but the reindex is**, so a crash mid-scan leaves the library updated and the index empty.

### 2.5 Schema evolution is half-implemented

This one has moved since the last audit, and the movement is instructive.

`migrate()` now does contain an `ALTER TABLE` path with a `hasColumn` guard — the `capabilities.scope` column was added this way. That is a real improvement. But it is **a hand-written one-off**, not a migration system: there is no ordered migration list, no down-path, no guard against running older code against a newer database, and no record of *which* migrations ran beyond a single version integer.

**The next column anyone adds will be added by hand again**, in the same style, and the one after that differently. Replace this with an ordered migration mechanism now, while there is exactly one migration to port — not later, when there are six and they must be untangled.

### 2.6 Structural drag

**The scanner knows about protocols.** It is a five-branch `if / else if` over `src.kind` — still five branches — each re-implementing directory walking. Adding a sixth protocol means editing the scanner, the source-kind enum, the `KINDS` array, the renderer's kind map, and the filter buttons. Five edit sites for one concept. The `adapters/` directory exists and is called, but **from inside the branches** rather than as the extension point.

**The IPC contract is maintained in three places by hand.** As of this version: **68 handler registrations, 68 preload methods, 69 typed wrappers**, with nothing linking them. A renamed channel fails at runtime as a toast, not at build time. This is the interface between every layer in the app, and it is unverified.

**Fourteen views each hand-roll their own loading / error / reload state.** There is no shared server-state layer. Independent polling loops run against `/status` and scan-busy at different intervals.

**Business logic sits in the Electron main process.** Boot scan, schedules, settings reads — all of it in `main.js`, none testable without a window.

**Two new subsystems inherited the same pattern.** The model-integration service and the asset service are correct and reasonably placed, but they were added to the same single process with the same synchronous driver. Good features, same structural debt.

### 2.7 Smaller things worth a glance

Not urgent. Listed so you are not surprised, not because they deserve priority.

- A hand-rolled Markdown renderer escapes `&`, `<`, `>` but **not quotes**, then interpolates captured groups into `href="..."` and feeds the result to `dangerouslySetInnerHTML`. Capability bodies are untrusted input. I did not demonstrate an exploit; the shape is an attribute-injection seam.
- Two different content "hashes": snapshots record `sha256(String(Date.now()))`, the ecosystem backup uses a 32-bit rolling hash, and the fault backup uses a real SHA-256. **Same table, three semantics.**
- `walkFiles` stops silently at 500 files with no report. `depth` means different things to different collectors.
- A dead loop in `agents.scan` whose guard is `ts - 5*60*1000 > ts`, always false.
- The built renderer bundle is committed **next to** the source, and `main.js` loads it whenever dev mode is off, so a stale bundle silently becomes the running UI. This bit me during this very audit.
- No linter, no unused-import detection.
- The oversize-body test asserts a status code that is the **opposite** of what the test is for.
- The provider API key is read from `process.env.CC_MODEL_API_KEY` at module load, so rotating it requires a restart. Workable; worth knowing.

### 2.8 What is already fixed — do not re-report

Verified against the current code. If you find these back, the repository has regressed and you should say so loudly.

- **Empty-string bind mismatch — fixed.** `listCapabilities({ kind: '', status: 'active' })` now returns normally (200 rows) instead of throwing `column index out of range`. The bind filter was hardened.
- **Hard-coded machine paths — fixed.** The upstream catalogue cache resolves under the user home (`services/updates.js`); the extra-library path is gone; backup fallbacks no longer embed an absolute path; the seeded MCP example uses a relative workspace.
- **Diagnostic rule count — no longer claimed in prose.** The README describes the categories rather than a number, and there are nine rules.
- **Schema `ALTER TABLE` path — present.** See §2.5 for what remains missing.

### 2.9 The `old/` directory is expendable history, not code you must keep

There is an `old/` directory at the repository root containing the previous-generation
implementation: a Tauri 2 desktop shell over a six-crate Rust workspace, with Tantivy
full-text search and a slot-based model configuration. It is **frozen and abandoned**.

It is included for one reason: it holds the design reasoning that led to the current
architecture, and a few lessons that survived the rewrite. It is **not** a reference
implementation, and nothing in the current product imports from it.

**It is explicitly yours to delete.** If, after reading it, you conclude any of the following:

- the useful parts are now covered by `docs/` and the current source;
- it clutters the repository for readers who came here to work on the live code;
- or keeping a dead tree in the repository is simply the wrong way to preserve history
  (Git history already holds it);

then **remove `old/` entirely**. You do not need permission, and I am not asking you to
consult me. The same goes for replacing it with a shorter summary, moving it to a separate
repository, or archiving it behind a tag.

What is actually worth salvaging from it, if you want a shortlist:

| Path in `old/` | Why it might matter |
| --- | --- |
| `kernel/crates/ac-core/` | The protocol-layer data model — the earliest carefully designed part; the current Manifest traces back to it |
| `docs/lessons/` | Rewrite-surviving traps: Tauri dev/protocol mode, manifest embedding, path parsing |
| `docs/planning/03-里程碑设计存档-M2至M6.md` | The original six-layer architecture reasoning |

Everything else in there is superseded. **Do not build new work on top of it.**

---

## 3. Where the weight actually sits

This section is opinion, and you will have better information than I do once you are inside the code.

If I had to name the highest-leverage thing in this repository, it is not any item in §2:

**The division of labour — hub diagnoses, agent repairs, hub verifies — is the only idea here I have not seen elsewhere, and it is the least implemented.**

Everything needed to make it real already has a stub: a fault table with evidence, a dispatch action that generates a structured work order, an MCP tool that accepts a patch back, a repair log with before and after hashes, and a backup system. What is missing is what turns a procedure into a protocol: **ownership, a state machine, verification that re-runs the check, and a conflict story when two agents are working.**

If you did nothing this round but make that loop trustworthy end to end, and left every performance number where it is, I would call the round a success. **I am aware that might be the wrong call.** That is why it is a paragraph and not a requirement.

Second highest: **the trust boundary from §2.1.** Not because the individual findings are severe in a single-user desktop app — they are moderate — but because "the hub writes to files it does not own" is the exact claim the product makes, and a boundary one level weaker than its description will eventually be load-bearing.

Third: **the process split.** It makes everything else cheaper and is the most likely to be over-done. See §5.

I rank performance and schema work lower — not because they are unimportant, but because they are mechanical and you will get them right. **The ranking above is about where judgement is required.**

---

## 4. What I think good looks like, and my uncertainty about it

Stated as outcomes, not implementations. Argue with any of them.

- **The repair loop is a protocol, not a procedure.** A ticket has an owner and a state. A dead worker is distinguishable from a slow one. A patch that does not fix the fault is recorded as a failure and the fault stays open. Two agents racing on one ticket produce one winner and an honest log.
- **Every advertised capability is asserted.** The retrieval channel exists or it does not. The settings toggle does something or it is not there. Whatever structure makes those two statements cheap to enforce is worth more than the individual fixes.
- **Nothing writes outside a path it can name in advance.** One guard, used everywhere, that a reviewer can check by grep rather than by reasoning.
- **The kernel can be tested without a window.** Not "should be"; can be, today, by someone who has never seen Electron.
- **Adding a protocol, an endpoint, or a channel is a small local change.** If it is five edits in five files, the abstraction is missing, not the discipline.
- **The corpus can grow an order of magnitude without the UI noticing.** I have deliberately not said how; the constraint is the outcome.
- **A model-backed feature degrades to a local one without the caller knowing.** This already works for embeddings and reranking. Keep it that way as you add roles; a cloud dependency that can fail a retrieval is worse than no cloud feature.

Where I am unsure, off the top of my head:

- **Whether the process split is worth what it costs.** It buys testability and responsiveness, and costs a supervisor, a control channel, a failure model, and a class of bugs that appear only when two processes disagree about state. In a desktop app, the second process is also a second thing to orphan. I think it is worth it, but I would want it **argued rather than assumed**, and doing it late — after the layering exists — is far cheaper than doing it early.
- **Whether the trigram channel is worth maintaining.** It is the right tool for Chinese substring matching, and SQLite's `trigram` does not match queries shorter than three characters — for Chinese that is most of them. Keeping it, replacing it with a bigram scheme, or dropping it and documenting the limitation are all defensible. I do not know which is right without measurements I did not take.
- **Whether the frontend should get a component framework.** Fourteen views of hand-rolled state is painful, but the current shape is also small, dependency-free and fast. I lean towards "add a server-state layer and a real Markdown parser, keep the rest" — a preference, not a finding.
- **Whether the asset library should be the primary model or a special case of the capability model.** It currently is the latter, distinguished by a `scope` column. That was the cheapest thing to build. It may not be the clearest thing to live with.

---

## 5. What I would do, offered as one plan among several

You have no obligation to follow this. I include it because a handover with no opinion is less useful than one with a stated opinion you can reject.

My instinct is to sequence by **what makes later work cheaper**, not by severity:

1. **Pin current behaviour with tests before changing anything.** Four paths have no coverage and are the ones I would least want to refactor blind: the patch write, the auto-fix write, the HTTP auth surface, and the trigram query. Characterisation tests first make every later change safe to attempt.
2. **Make the boundary real.** One path guard, one token check, one Host check, used uniformly. Small, independent of everything else, and it removes a class of problem from your mental load for the rest of the round.
3. **Make the loop real.** Ticket ownership, state machine, verification, conflict. This is the round's real work and it wants the tests from step 1.
4. **Make the schema evolvable, then the data layer fast enough.** An ordered migration mechanism first, because you cannot add columns reliably without it — and there is exactly one migration to port today. Then transactions, statement cache, content storage, retention.
5. **Split the layering, then split the process.** Layering first because it is cheaper and makes the split mechanical. The split last because it is a deployment change with its own failure modes; doing it after everything else means debugging one thing at a time.
6. **Then the frontend, then retrieval quality.** Both benefit from everything above; neither blocks anything below.

Two things I would deliberately **not** do early: chase performance before the schema work, and touch the frontend before the kernel boundary exists. Both are rework generators.

Where I could easily be wrong: steps 2 and 3 can run in parallel if you have the appetite for two threads. And if you conclude the process split is not worth it, steps 1–4 stand on their own and lose very little.

---

## 6. The decision space — explicitly yours

This section exists because I do not want to be the person who decided these by omission. **All of it is yours. I hold no veto and claim none.**

**Not decided by me, and I would rather you decided:**

- Whether to split the process at all, and into how many.
- Whether to adopt any framework or library, and which. My §4 opinions are weak.
- Whether to rewrite or refactor the frontend data layer, the Markdown renderer, the search ranking, or the workflow engine.
- Whether to keep, extend, replace or delete the trigram channel.
- Whether the repair protocol should be a state machine, an event log, a lease table, or something I have not thought of.
- What the schema should look like when you are done.
- Which of my §2 findings you consider real, and which you dismiss.
- Whether the round should be organised by subsystem, by risk, or by user-visible outcome.
- What "done" means, beyond the outcomes in §4.
- Whether anything in §5 is worth following.
- **Whether this should remain one application at all.** A hub that speaks MCP has no obligation to own a GUI. Splitting the kernel into a headless service and shipping the UI as one client among several is a legitimate reading of this codebase, and I have not ruled it out — I simply did not do it.
- **Whether `old/` should exist at all.** See §2.9. Deleting it, shrinking it to a paragraph, or moving it elsewhere are all fine. I have no claim on it.

**Constraints I would push back on only if you have a reason:**

These four are genuine constraints rather than preferences, because breaking them breaks something a user already has. Everything else is negotiable.

- An existing database keeps its rows through any migration.
- MCP tool names and REST paths keep working with their current shapes. Additions are fine; silent changes are not.
- The service stays loopback-only, and no new network call runs without the user starting it.
- A write to a file the hub does not own has a backup that can be restored from the UI.

If you believe one of those should be broken, make the case — say what it buys, what it costs, and how to get back. **I would be glad to be overruled, provided it is argued rather than assumed.**

**Explicitly not urgent this round** (unless you think otherwise and say so): installers, code signing, release versioning, hosted CI, cloud sync, account systems, telemetry.

---

## 7. What would help me most in your reply

Not a required format. This is what I would find most useful, roughly in priority order.

1. **Which of my claims are wrong**, with the file and line where you checked. I would rather learn that §2.1 was overstated than read a confirmation.
2. **What I missed** that is worse than anything I found.
3. **What you decided against me on**, and why. The judgements in §3–§5 are the parts I most expect to be corrected.
4. **What you actually did**, with the command that shows it working. For anything you chose not to do, one line on why.
5. **What you are still unsure about** when you finish. Honest residual risk is more useful than a clean summary.

If you disagree with the shape of this document — too long, too prescriptive, wrong about what matters — **say so and work from your own reading.** I would rather receive the upgrade you think is right with a paragraph explaining why you ignored me, than a faithful execution of a brief I am not confident in.

---

## Appendix A — Reproduction

Each check reproduces one claim from §2. Run them against a **copy** of the database or a throwaway context, never the live one. If a check does not reproduce, record that and move on; **the disagreement is more informative than the fix.**

**A-1 · repair ticket escapes its installation.** Create two skills that both lack frontmatter in one source directory. Register the directory as a `skill` source at depth 1, run `scanAll({})`, then `diagnose()`. A `skill-missing-frontmatter` fault will exist for the first skill. Call `submitPatch` with that `faultId` and `file` pointing at the **other** skill's entry file.

*Observed on current code:* no error; the other skill's file is overwritten with the submitted content. *Expected after a fix:* throws, file unchanged.

**A-2 · no authentication, open CORS.** Start the HTTP server on an ephemeral port against a temporary context.

```
curl -i -H "Host: attacker.example" http://127.0.0.1:<port>/api/v1/capabilities
curl -i -X OPTIONS http://127.0.0.1:<port>/api/v1/status
```

*Observed:* `200` with the full list; `204` for the preflight; `access-control-allow-origin: *` on both.

**A-3 · empty retrieval channel.** On a database built by a full scan:

```sql
SELECT COUNT(*) FROM search_docs;      -- observed 310
SELECT COUNT(*) FROM search_fts;       -- observed 310
SELECT COUNT(*) FROM search_trigram;   -- observed 0
```

Then confirm nothing writes to it: the only references in the source tree are `db/schema.js` (create) and `services/search.js` (query).

**A-4 · empty-string bind mismatch — now fixed.** With at least one capability present, call `listCapabilities({ kind: '', status: 'active' })`.

*Observed on current code:* returns normally. *Historical behaviour:* threw `column index out of range`. If you see the throw return, the fix regressed.

**A-5 · measured baselines.** On a database built by a full scan of a real corpus: `dedupe.scan` 433 ms, `reindexSearch` 240 ms, `search` 5 ms. Re-measure whatever you touch; these numbers are a machine, not a specification.

**A-6 · unwritten setting.** `grep -rn "diagnoseOnScan" .`

*Observed:* one hit, in `renderer/views/SettingsView.tsx`. No reader in the main process.

**A-7 · IPC contract drift.** Count the three surfaces and compare:

```
grep -c "register('"     apps/desktop/src/main/main.js
grep -c "invoke('"       apps/desktop/src/main/preload.cjs
grep -c "call<"          apps/desktop/src/renderer/lib/api.ts
```

*Observed:* 68 / 68 / 69. Nothing links them; a mismatch is a runtime toast, never a build error.

**A-8 · verification is a label.** Read `services/faults.js` around the `autoFix` and `submitPatch` write paths. Confirm neither re-runs the diagnostic rule that raised the fault before setting `verify_state='verified'`. Then write a patch that changes nothing and observe it recorded as a resolved, verified repair.

**A-9 · documentation drift.** Pick any number stated in prose and compare it against the code. The historical example — a claimed rule count that did not match — has been corrected, which means the exercise now needs a fresh target. Find one.

---

*End of handover. The plan in §5 is a suggestion. The findings in §2 are claims. The judgement is yours — and from here on, it is the community's.*
