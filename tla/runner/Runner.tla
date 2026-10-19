------------------------------ MODULE Runner ------------------------------
(***************************************************************************)
(* tlcq's VM runner (openspec changes tlc-queue D9, D10, and pinned-slots). *)
(* Runs of job ids wait in a queue on disk and run in attempts, each in    *)
(* its own unit (named by id, run and attempt) and directory. A unit here  *)
(* is a podman container on pve, the stop hook the runner container        *)
(* waiting on it, and boot the runner's start; a runner that dies while    *)
(* containers run is a hook missed and made good by later settling. Two    *)
(* slots, E and P, one job each; a human may close P (a job already in it  *)
(* runs on). Slot use is derived from the files: an attempt with no end    *)
(* holds its slot unless it is frozen.                                     *)
(* Freeze (the page): the container is paused and its slot freed. A       *)
(* frozen job thaws on spare capacity (a free open slot, nothing waiting)  *)
(* and holds that slot, but is frozen again when a run wants a slot and    *)
(* none is free; closing P freezes one thawed there. Unfreeze makes a      *)
(* frozen job wait first for the next free slot, a thawed one ordinary.    *)
(* Pause (the page): a request in the attempt's directory; the supervisor  *)
(* of a running job, at a checkpoint, writes its paused stop marker under  *)
(* the lock and stops its engine, then keeps the checkpoint; a frozen job  *)
(* is unpaused for it and stops at once, with its last checkpoint or with  *)
(* none (it ends `cancelled`). Settling ranks the stop marker, never the   *)
(* request, and only with the checkpoint kept: the run is then paused, in  *)
(* no slot and not queued, until a resume queues its next attempt.         *)
(* Keep running (the page) withdraws the request of a job pausing in its   *)
(* slot, under the lock and before its paused marker; the supervisor       *)
(* rechecks the request under the lock as it writes the marker.            *)
(* A resume that dies with nothing to say (a shutdown, or out of memory as *)
(* it reloads its queue) leaves the run paused again on that checkpoint.   *)
(* A whole-VM run (a memory request above one slot's share) starts only in *)
(* E and only while no job holds P; while it has an attempt with no end    *)
(* (running, thawed, frozen, unfrozen or pausing) P counts as closed, and  *)
(* a thawed job in either slot is frozen again for one waiting.            *)
(* Every verb holds the admission lock; writing verbs (commit, status,    *)
(* cancel, the page's) and boot first settle each recorded attempt whose   *)
(* unit is gone and that has no end, then apply, then admit. Admission     *)
(* records an attempt under a number never used before in its run, then    *)
(* starts the container, which may fail. A supervisor writes a started     *)
(* marker first; it makes cap and pause decisions under the lock and stops *)
(* its own engine. A stopped unit's hook settles under the lock in steps   *)
(* (cache, end, scratch, admission) and may be killed before or between    *)
(* any two. A gone attempt without the started marker is undone, its job  *)
(* queued again (a resumed one at the head), unless it holds a cancel      *)
(* marker, which ends the run `cancelled`. The NeverStarted-th undo in a   *)
(* row (a started marker resets the count, a failed start neither counts   *)
(* nor resets) ends the run `error` ("never started"), a run-level outcome *)
(* kept with its queue entry. (The design writes run-level outcomes into   *)
(* the last recorded attempt's result.json; the model keeps them in `qe`,  *)
(* and a cancel of a paused run overwrites that attempt's end. Equivalent  *)
(* here: RunOutcome reads `qe` first, and an attempt with an end is never  *)
(* settled again.) The VM may crash: every unit is gone and no hook runs;  *)
(* boot settles, deletes scratch and admits.                               *)
(* Read-only verbs (preflight, `ls`, `fetch`) write nothing: PreflightTrue *)
(* checks what they report.                                                *)
(*                                                                         *)
(* Left out: attempt numbers past MaxAttempts (a run that has used them    *)
(* all, by failed starts, is outside the bound, so Quiet treats it as      *)
(* done; NeverStarted ≤ MaxAttempts, so undos end a run within it);        *)
(* telling a killed admission (the start never returned) from a unit gone  *)
(* before its started marker (both count as undos); entries and attach (an *)
(* attach changes no VM state); lanes, probes and times (a cap or a        *)
(* checkpoint may fall due at any moment, frozen time is not modelled);    *)
(* "oldest first" and "E first" (admission picks freely among what the     *)
(* slot rules allow, which is stricter than "at the head"); memory (a run  *)
(* short of it waits like one with no slot); `now-or-refuse` (a refused    *)
(* job is removed, as if never committed); the disk floor and the held     *)
(* queue (admission may stop at any point); shutdown (a crash is the       *)
(* harder case); the reply, built under the lock and written after it.     *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Ids,               \* job ids
          MaxRuns,           \* runs per id: a job ended, or cancelled and still stopping, runs anew
          MaxCommits,        \* runs committed in all
          MaxAttempts,       \* attempt numbers per run: a pause and an undone resume need 3
          MaxCrashes,        \* VM crashes
          MaxCancels,        \* `cancel`s and supersedes
          MaxUi,             \* the page's freezes, unfreezes, pauses, keeps, resumes and P toggles
          NeverStarted,      \* undos in a row that end a run `error` ("never started")
          StartFails,        \* a container start may fail, a unit may die before its started
                             \* marker (off only to bound a config)
          UndoOverCancel,    \* bug: a never-started attempt is undone even with a cancel marker
          ReuseAttempt,      \* bug: admission reuses an undone or failed attempt's number
          NoLimit,           \* bug: no never-started limit, a run is undone without end
          StopBeforeMark,    \* bug: the supervisor stops the engine before its paused marker
          NoUndoOnFail,      \* bug: a failed start leaves its record and directory
          NoStartedMarker,   \* bug: no started marker; a never-started attempt settles as `error`
          PausedOverCancel,  \* bug: settling ranks the paused marker above the cancel marker
          ResettleEnded,     \* bug: settling also takes attempts whose end is recorded
          PauseByRequest,    \* bug: settling ranks the page's pause request, not the stop marker
          NoCkCheck,         \* bug: settling takes the paused marker without the kept checkpoint
          ThawedFree,        \* bug: a thawed job's slot counts as free
          KeepAfterMark,     \* bug: keeping a pausing job running ignores its paused marker
          NoKeepOnDeath,     \* bug: a resume that died ends the run `error`, its checkpoint lost
          PreflightUnlocked, \* bug: the preflight reads without the admission lock
          NoSettle,          \* bug: writing verbs do not settle first (boot still does)
          Whole,             \* the ids whose memory request needs the whole VM (job-memory M2)
          WholeNoWait        \* bug: a whole-VM run starts in E with a job still holding P

VARIABLES
    up, crashes,  \* the VM is up (FALSE: crashed, not yet booted); crashes so far
    cancels,      \* cancels and supersedes so far
    ui,           \* the page's actions so far
    lk,           \* the admission lock: "free", or held by a "verb", "boot" or a stop "hook"
    hk,           \* {the attempt whose hook holds the lock}, else {}
    ph,           \* the holder's step: "apply", "cache", "end", "scratch", "admit", "run", "none"
    ac,           \* {the attempt admission recorded, its start not yet returned}, else {}
    runs,         \* [Ids -> 0..MaxRuns]: runs committed so far
    qe,           \* [RS -> "none" | "queued" | "started" | "cancelled" | "error"]: the run's queue
                  \* entry; cancelled and error are run-level outcomes kept with it
    und,          \* [RS -> Nat]: undos in a row (since an attempt last wrote its started marker)
    rq,           \* [RS -> 0..1]: the resumed run's next attempt, queued at the head, not yet started
    nxt,          \* [RS -> 0..MaxAttempts]: attempt numbers the run has used
    cxl,          \* [RS -> 0..Never]: attempt numbers used when the run was cancelled, else Never (ghost)
    rec,          \* [Att -> BOOLEAN]: recorded in its run's state, with its directory and first result.json
    end,          \* [Att -> "none" | Outcomes]: the end recorded in its result.json
    unit,         \* [Att -> "absent" | "live" | "stopping" | "post"]; post: stopped, its hook to run
    mk,           \* [Att -> SUBSET Markers]: markers in its directory
    out,          \* [Att -> BOOLEAN]: the engine's own outcome written to result.json
    ck,           \* [Att -> BOOLEAN]: its checkpoint kept in the run's checkpoint directory
    scr,          \* [Att -> BOOLEAN]: scratch present
    slot,         \* [Att -> "none" | "e" | "p"]: the slot it started or last thawed in
    fz,           \* [Att -> Freeze]: from its frozen.log and pause request
    pc,           \* the P slot is open
    pk,           \* attempts holding P when it closed (they run on), else {} (ghost)
    died,         \* attempts that ran, then crashed or died with nothing to say otherwise (ghost)
    dec,          \* attempts whose supervisor decided a pause (ghost; StopBeforeMark only)
    cached,       \* ids with a cached result
    late          \* attempts stopped, their paused marker not yet written (StopBeforeMark)

vars ≜ ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, unit,
        mk, out, ck, scr, slot, fz, pc, pk, died, dec, cached, late⟩

Runs ≜ 1..MaxRuns
Atts ≜ 1..MaxAttempts
RS ≜ Ids × Runs
Att ≜ Ids × Runs × Atts
Never ≜ MaxAttempts + 1
At(x, k) ≜ ⟨x[1], x[2], k⟩
RunOf(a) ≜ ⟨a[1], a[2]⟩
AttsOf(x) ≜ {At(x, k) : k ∈ Atts}
The(S) ≜ CHOOSE e ∈ S : TRUE

Slots ≜ {"e", "p"}
\* "pause" is the page's request; "paused" the supervisor's stop marker; "nockpt" its stop of a
\* frozen job that never completed a checkpoint.
Markers ≜ {"started", "cancel", "pause", "paused", "nockpt", "timeout"}
Outcomes ≜ {"cancelled", "done", "paused", "timeout", "error"}
Ending ≜ Outcomes \ {"paused"}
\* run: running in its slot; frozen; thawed: on spare capacity, holding its slot; unfr: frozen,
\* unfrozen, waiting first for a slot; pz: a frozen job unpaused for its pause, in no slot.
Freeze ≜ {"run", "frozen", "thawed", "unfr", "pz"}

(***************************************************************************)
(* Settling. S is a view of the files: the current one (Cur), or one       *)
(* already settled, so that settling twice can be checked.                 *)
(***************************************************************************)
Cur ≜ [e ↦ end, q ↦ rq, c ↦ cached, r ↦ rec, m ↦ mk, o ↦ out, k ↦ ck, s ↦ slot, x ↦ scr,
       u ↦ qe, n ↦ und]

PausedIn(S, a) ≜ IF PauseByRequest THEN "pause" ∈ S.m[a]
                 ELSE "paused" ∈ S.m[a] ∧ (S.k[a] ∨ NoCkCheck)

\* An earlier attempt of its run kept the run's checkpoint: the attempt recovered it.
RunKept(S, a) ≜ ∃ k ∈ Atts : k < a[3] ∧ S.k[At(RunOf(a), k)]

\* First match wins: cancel marker, the engine's own outcome, paused (with its checkpoint kept),
\* paused before a first checkpoint, timed out, paused again with the run's checkpoint when a
\* resume died with nothing to say (its checkpoint is kept for another resume), else error.
OutcomeIn(S, a) ≜
    CASE "cancel" ∈ S.m[a] ∧ ¬(PausedOverCancel ∧ PausedIn(S, a)) → "cancelled"
      □ S.o[a] → "done"
      □ PausedIn(S, a) → "paused"
      □ "nockpt" ∈ S.m[a] → "cancelled"
      □ "timeout" ∈ S.m[a] → "timeout"
      □ RunKept(S, a) ∧ ¬NoKeepOnDeath → "paused"
      □ OTHER → "error"
Outcome(a) ≜ OutcomeIn(Cur, a)

\* A recorded attempt whose unit is gone and that has no end.
Sble(S, a) ≜ S.r[a] ∧ unit[a] = "absent" ∧ (S.e[a] = "none" ∨ ResettleEnded)
Ran(S, a) ≜ "started" ∈ S.m[a] ∨ NoStartedMarker
\* Never ran, and no cancel marker (which ends the run `cancelled`): deleted like a failed
\* start, its job queued again.
Unrun(S, a) ≜ ¬Ran(S, a) ∧ (UndoOverCancel ∨ "cancel" ∉ S.m[a])
Undo(S, a) ≜ Sble(S, a) ∧ Unrun(S, a)
Fin(S, a) ≜ Sble(S, a) ∧ ¬Unrun(S, a)
\* A resume's attempt: an earlier attempt of its run is recorded (attempt numbers have gaps).
IsResume(S, a) ≜ ∃ k ∈ Atts : k < a[3] ∧ S.r[At(RunOf(a), k)]

\* The never-started limit: this undo is the run's NeverStarted-th in a row, so the run ends
\* `error` (a run-level outcome, kept with its queue entry), its queued resume dropped, and no
\* attempt's end changed.
AtLimit(n) ≜ ¬NoLimit ∧ n + 1 ≥ NeverStarted

Settle(S) ≜
    LET e2 ≜ [a ∈ Att ↦ IF Fin(S, a) THEN OutcomeIn(S, a) ELSE S.e[a]]
        undone ≜ {x ∈ RS : ∃ k ∈ Atts : Undo(S, At(x, k))}
        limit ≜ {x ∈ undone : AtLimit(S.n[x])}
        requeue ≜ {x ∈ RS : ∃ k ∈ Atts : Undo(S, At(x, k)) ∧ IsResume(S, At(x, k))}
    IN [e ↦ e2,
        q ↦ [x ∈ RS ↦ IF x ∈ limit THEN 0 ELSE IF x ∈ requeue THEN 1 ELSE S.q[x]],
        n ↦ [x ∈ RS ↦ IF x ∈ undone THEN S.n[x] + 1 ELSE S.n[x]],
        c ↦ S.c ∪ {a[1] : a ∈ {b ∈ Att : Fin(S, b) ∧ OutcomeIn(S, b) = "done"}},
        r ↦ [a ∈ Att ↦ S.r[a] ∧ ¬Undo(S, a)],
        m ↦ [a ∈ Att ↦ IF Undo(S, a) THEN {} ELSE S.m[a]],
        o ↦ [a ∈ Att ↦ S.o[a] ∧ ¬Undo(S, a)],
        k ↦ S.k,
        s ↦ [a ∈ Att ↦ IF Undo(S, a) THEN "none" ELSE S.s[a]],
        \* and the scratch of ended (or undone) attempts is deleted
        x ↦ [a ∈ Att ↦ S.x[a] ∧ ¬(unit[a] = "absent" ∧ (Undo(S, a) ∨ e2[a] ≠ "none"))],
        u ↦ [x ∈ RS ↦ IF x ∈ limit THEN "error"
                       ELSE IF ∃ k ∈ Atts : Undo(S, At(x, k)) ∧ ¬IsResume(S, At(x, k))
                       THEN "queued" ELSE S.u[x]]]

Settled ≜ Settle(Cur)
UndoneNow ≜ {a ∈ Att : Undo(Cur, a)}

Apply(S) ≜
    ∧ end' = S.e ∧ rq' = S.q ∧ cached' = S.c ∧ rec' = S.r ∧ mk' = S.m ∧ out' = S.o
    ∧ slot' = S.s ∧ qe' = S.u ∧ und' = S.n
    ∧ died' = died \ UndoneNow ∧ dec' = dec \ UndoneNow

\* The run's last recorded attempt's number, else 0.
LastIn(S, x) ≜ LET ks ≜ {k ∈ Atts : S.r[At(x, k)]} IN
               IF ks = {} THEN 0 ELSE CHOOSE k ∈ ks : ∀ k2 ∈ ks : k2 ≤ k
Last(x) ≜ LastIn(Cur, x)

\* A run's outcome: a run-level one (cancelled while queued, never started), else its last started
\* attempt's, once that is no pause.
RunOutcome(S, x) ≜
    IF S.u[x] ∈ {"cancelled", "error"} THEN S.u[x]
    ELSE LET n ≜ LastIn(S, x) IN
         IF n > 0 ∧ S.e[At(x, n)] ∈ Ending THEN S.e[At(x, n)] ELSE "none"

\* Paused: its last attempt ended paused, and no resume is queued.
PausedRun(x) ≜ qe[x] = "started" ∧ rq[x] = 0 ∧ Last(x) > 0 ∧ end[At(x, Last(x))] = "paused"

\* The number admission gives a run's next attempt: never one used before, an undone or failed
\* attempt's included (ReuseAttempt: the one after the last recorded).
Alloc(x) ≜ IF ReuseAttempt THEN Last(x) + 1 ELSE nxt[x] + 1

(***************************************************************************)
(* Slots and admission.                                                    *)
(***************************************************************************)
NoEnd(a) ≜ rec[a] ∧ end[a] = "none"
Holds(a) ≜ fz[a] = "run" ∨ (fz[a] = "thawed" ∧ ¬ThawedFree)
Occ(s) ≜ {x ∈ RS : ∃ a ∈ AttsOf(x) : NoEnd(a) ∧ slot[a] = s ∧ Holds(a)}
\* A whole-VM run with an attempt that has no end holds P; a paused one has none.
WholeHold ≜ ∃ a ∈ Att : a[1] ∈ Whole ∧ NoEnd(a)
Open(s) ≜ s = "e" ∨ (pc ∧ ¬WholeHold)
Free(s) ≜ Open(s) ∧ Occ(s) = {}
\* A whole-VM run starts only in E, with no job holding P (closed by a human or not).
WholeOK(x, s) ≜ x[1] ∈ Whole ⇒ s = "e" ∧ (WholeNoWait ∨ Occ("p") = {})

\* A frozen container the page or admission can act on.
Paused(a) ≜ NoEnd(a) ∧ unit[a] = "live" ∧ "cancel" ∉ mk[a]

Waiting(x) ≜
    ∧ Alloc(x) ≤ MaxAttempts
    ∧ qe[x] = "queued" ∨ (rq[x] > 0 ∧ ∀ a ∈ AttsOf(x) : ¬NoEnd(a))
Unfrozen ≜ {a ∈ Att : Paused(a) ∧ fz[a] = "unfr"}
Wanted ≜ (∃ x ∈ RS : Waiting(x)) ∨ Unfrozen ≠ {}

FreshOK(x, s) ≜ qe[x] = "queued" ∧ Alloc(x) ≤ MaxAttempts ∧ Free(s) ∧ WholeOK(x, s)
ResumeOK(x, s) ≜ rq[x] > 0 ∧ Alloc(x) ≤ MaxAttempts ∧ (∀ a ∈ AttsOf(x) : ¬NoEnd(a)) ∧ Free(s)
                 ∧ WholeOK(x, s)
WholeWants ≜ ∃ x ∈ RS : x[1] ∈ Whole ∧ Waiting(x)
\* An unfrozen job takes a free slot; a frozen one only spare capacity.
ThawOK(a, s) ≜
    ∧ Paused(a) ∧ Free(s)
    ∧ ∨ fz[a] = "unfr"
      ∨ fz[a] = "frozen" ∧ ¬(∃ x ∈ RS : Waiting(x)) ∧ Unfrozen = {}
RefreezeOK(a) ≜
    ∧ Paused(a) ∧ fz[a] = "thawed" ∧ "pause" ∉ mk[a]
    ∧ ∨ Wanted ∧ (∀ s ∈ Slots : ¬Free(s)) ∧ Open(slot[a])
      \* (A whole-VM run waits for E and an empty P: a thawed job in either is in its way.)
      ∨ WholeWants

CanAdmit ≜ ∨ ∃ x ∈ RS, s ∈ Slots : ResumeOK(x, s) ∨ FreshOK(x, s)
           ∨ ∃ a ∈ Att : RefreezeOK(a) ∨ ∃ s ∈ Slots : ThawOK(a, s)

\* The lock is freed; a hook's unit is gone once its hook ends.
Release ≜
    ∧ unit' = IF lk = "hook" THEN [unit EXCEPT ![The(hk)] = "absent"] ELSE unit
    ∧ lk' = "free" ∧ hk' = {} ∧ ph' = "none" ∧ ac' = {}

\* Record the attempt in its run's state, with its directory and first result.json.
AdmitRecord ≜
    ∧ up ∧ lk ≠ "free" ∧ ph = "admit"
    ∧ ∃ x ∈ RS, s ∈ Slots :
        LET a ≜ At(x, Alloc(x)) IN
        ∧ ∨ ResumeOK(x, s) ∧ rq' = [rq EXCEPT ![x] = 0] ∧ UNCHANGED qe
          ∨ FreshOK(x, s) ∧ qe' = [qe EXCEPT ![x] = "started"] ∧ UNCHANGED rq
        ∧ nxt' = [nxt EXCEPT ![x] = IF a[3] > @ THEN a[3] ELSE @]
        ∧ rec' = [rec EXCEPT ![a] = TRUE]
        ∧ slot' = [slot EXCEPT ![a] = s]
        ∧ fz' = [fz EXCEPT ![a] = "run"]
        ∧ ac' = {a}
    ∧ ph' = "run"
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, runs, und, cxl, end, unit, mk, out, ck, scr,
                 pc, pk, died, dec, cached, late⟩

\* `podman update --cpuset-cpus`, `podman unpause` and the thaw in frozen.log, in one step.
AdmitThaw ≜
    ∧ up ∧ lk ≠ "free" ∧ ph = "admit"
    ∧ ∃ a ∈ Att, s ∈ Slots :
        ∧ ThawOK(a, s)
        ∧ fz' = [fz EXCEPT ![a] = IF @ = "unfr" THEN "run" ELSE "thawed"]
        ∧ slot' = [slot EXCEPT ![a] = s]
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end,
                 unit, mk, out, ck, scr, pc, pk, died, dec, cached, late⟩

AdmitRefreeze ≜
    ∧ up ∧ lk ≠ "free" ∧ ph = "admit"
    ∧ ∃ a ∈ Att : RefreezeOK(a) ∧ fz' = [fz EXCEPT ![a] = "frozen"]
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end,
                 unit, mk, out, ck, scr, slot, pc, pk, died, dec, cached, late⟩

\* The container start: the unit starts, or the call fails, the record and directory are deleted,
\* the job stays queued (a resume at the head), and this admission ends. A unit of the same name
\* still there makes it fail (only with ReuseAttempt: NoNameClash).
AdmitRun ≜
    ∧ up ∧ lk ≠ "free" ∧ ph = "run"
    ∧ LET a ≜ The(ac)  x ≜ RunOf(a) IN
       ∨ ∧ unit[a] = "absent"
         ∧ unit' = [unit EXCEPT ![a] = "live"]
         ∧ scr' = [scr EXCEPT ![a] = TRUE]
         ∧ ph' = "admit" ∧ ac' = {}
         ∧ UNCHANGED ⟨lk, hk, rec, slot, qe, rq⟩
       ∨ ∧ StartFails
         ∧ IF NoUndoOnFail THEN UNCHANGED ⟨rec, slot, qe, rq⟩
           ELSE ∧ rec' = [rec EXCEPT ![a] = FALSE]
                ∧ slot' = [slot EXCEPT ![a] = "none"]
                ∧ IF IsResume(Cur, a) THEN rq' = [rq EXCEPT ![x] = 1] ∧ UNCHANGED qe
                  ELSE qe' = [qe EXCEPT ![x] = "queued"] ∧ UNCHANGED rq
         ∧ Release
         ∧ UNCHANGED scr
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, runs, und, nxt, cxl, end, mk, out, ck, fz, pc, pk, died,
                 dec, cached, late⟩

\* Nothing more to admit, or the disk is below its floor, or the queue is held.
AdmitEnd ≜
    ∧ up ∧ lk ≠ "free" ∧ ph = "admit"
    ∧ Release
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, runs, qe, rq, und, nxt, cxl, rec, end, mk, out, ck, scr,
                 slot, fz, pc, pk, died, dec, cached, late⟩

(***************************************************************************)
(* Writing verbs: settle, apply, admit, all under the lock.                *)
(***************************************************************************)
LiveRun(x) ≜ qe[x] = "queued" ∨ rq[x] > 0 ∨ PausedRun(x)
             ∨ ∃ a ∈ AttsOf(x) : NoEnd(a) ∧ "cancel" ∉ mk[a]
Commits ≜ LET F[S ∈ SUBSET Ids] ≜ IF S = {} THEN 0 ELSE runs[The(S)] + F[S \ {The(S)}] IN F[Ids]
\* An id runs anew when it has no live run (a run cancelled and still stopping is not one).
CanNew(j) ≜ runs[j] < MaxRuns ∧ Commits < MaxCommits ∧ j ∉ cached ∧ ∀ r ∈ Runs : ¬LiveRun(⟨j, r⟩)
CanCancel ≜ cancels < MaxCancels ∧ ∃ x ∈ RS : LiveRun(x)

VerbTake ≜
    LET S ≜ Settled IN
    ∧ up ∧ lk = "free"
    ∧ ∨ ¬NoSettle ∧ S ≠ Cur
      ∨ ∃ j ∈ Ids : CanNew(j)
      ∨ CanCancel
      ∨ CanAdmit
      ∨ ui < MaxUi
    ∧ lk' = "verb" ∧ ph' = "apply"
    ∧ IF NoSettle THEN UNCHANGED ⟨end, rq, cached, rec, mk, out, slot, qe, scr, died, dec, und⟩
      ELSE Apply(S) ∧ scr' = S.x
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, hk, ac, runs, nxt, cxl, unit, ck, fz, pc, pk, late⟩

\* Cancel the runs Y (at most one): a queued run ends at once; a running or frozen one gets a
\* cancel marker and a stop (a frozen container unpaused and killed); a paused run ends, its queued
\* resume dropped, the outcome written into its last attempt's result.json. q0 is the queue after
\* the commit's own job.
Stops(Y, a) ≜ RunOf(a) ∈ Y ∧ NoEnd(a) ∧ "cancel" ∉ mk[a]
Drops(Y, a) ≜ RunOf(a) ∈ Y ∧ a[3] = Last(RunOf(a)) ∧ end[a] = "paused"
CancelVars(Y, q0) ≜
    ∧ qe' = [x ∈ RS ↦ IF x ∈ Y ∧ q0[x] = "queued" THEN "cancelled" ELSE q0[x]]
    ∧ mk' = [a ∈ Att ↦ IF Stops(Y, a) THEN mk[a] ∪ {"cancel"} ELSE mk[a]]
    ∧ unit' = [a ∈ Att ↦ IF Stops(Y, a) ∧ unit[a] = "live" THEN "stopping" ELSE unit[a]]
    ∧ end' = [a ∈ Att ↦ IF Drops(Y, a) THEN "cancelled" ELSE end[a]]
    ∧ rq' = [x ∈ RS ↦ IF x ∈ Y THEN 0 ELSE rq[x]]
    ∧ cxl' = [x ∈ RS ↦ IF x ∈ Y THEN nxt[x] ELSE cxl[x]]
    ∧ cancels' = cancels + Cardinality(Y)

\* A commit's supersede: another id's live run left with no entry.
Sup(j) ≜ {{}} ∪ (IF cancels < MaxCancels THEN {{y} : y ∈ {z ∈ RS : z[1] ≠ j ∧ LiveRun(z)}} ELSE {})

\* The page's verbs on an attempt. A pausing job is never frozen, thawed or unfrozen.
FreezeOK(a) ≜ Paused(a) ∧ Ran(Cur, a) ∧ "pause" ∉ mk[a] ∧ fz[a] ∈ {"run", "thawed"}
UnfreezeOK(a) ≜ Paused(a) ∧ "pause" ∉ mk[a] ∧ fz[a] ∈ {"frozen", "thawed"}
PauseOK(a) ≜ Paused(a) ∧ Ran(Cur, a) ∧ "pause" ∉ mk[a]
\* Keep running: a pause withdrawn while the job pauses in its slot, before its paused marker.
KeepOK(a) ≜ ∧ NoEnd(a) ∧ "pause" ∈ mk[a] ∧ fz[a] ∈ {"run", "thawed"} ∧ "cancel" ∉ mk[a]
            ∧ IF KeepAfterMark THEN unit[a] ∈ {"live", "stopping"} ELSE Paused(a) ∧ "paused" ∉ mk[a]

\* Closing P freezes a thawed job in it; the jobs left holding it run on.
Toggle ≜
    IF pc
    THEN LET f2 ≜ [a ∈ Att ↦ IF Paused(a) ∧ fz[a] = "thawed" ∧ slot[a] = "p" ∧ "pause" ∉ mk[a]
                             THEN "frozen" ELSE fz[a]] IN
         ∧ fz' = f2
         ∧ pk' = {a ∈ Att : NoEnd(a) ∧ slot[a] = "p" ∧ f2[a] ∈ {"run", "thawed"}}
         ∧ pc' = FALSE
    ELSE pc' = TRUE ∧ pk' = {} ∧ UNCHANGED fz

VerbApply ≜
    ∧ up ∧ lk = "verb" ∧ ph = "apply"
    ∧ ph' = "admit"
    ∧ ∨ \* commit: a new run of j (jobs before supersedes), then its supersede
        ∃ j ∈ Ids : ∃ Y ∈ Sup(j) :
          LET x ≜ ⟨j, runs[j] + 1⟩ IN
          ∧ CanNew(j)
          ∧ runs' = [runs EXCEPT ![j] = @ + 1]
          ∧ CancelVars(Y, [qe EXCEPT ![x] = "queued"])
          ∧ UNCHANGED ⟨ui, fz, pc, pk⟩
      ∨ \* cancel
        ∃ x ∈ RS : cancels < MaxCancels ∧ LiveRun(x) ∧ CancelVars({x}, qe)
                   ∧ UNCHANGED ⟨runs, ui, fz, pc, pk⟩
      ∨ \* the page
        ∧ ui < MaxUi ∧ ui' = ui + 1
        ∧ UNCHANGED ⟨runs, cxl, cancels, end, unit, qe⟩
        ∧ ∨ ∃ a ∈ Att : FreezeOK(a) ∧ fz' = [fz EXCEPT ![a] = "frozen"]
                        ∧ UNCHANGED ⟨mk, rq, pc, pk⟩
          ∨ ∃ a ∈ Att : UnfreezeOK(a) ∧ fz' = [fz EXCEPT ![a] = IF @ = "frozen" THEN "unfr" ELSE "run"]
                        ∧ UNCHANGED ⟨mk, rq, pc, pk⟩
          ∨ ∃ a ∈ Att : ∧ PauseOK(a)
                        ∧ mk' = [mk EXCEPT ![a] = @ ∪ {"pause"}]
                        ∧ fz' = [fz EXCEPT ![a] = IF @ ∈ {"frozen", "unfr"} THEN "pz" ELSE @]
                        ∧ UNCHANGED ⟨rq, pc, pk⟩
          ∨ ∃ a ∈ Att : KeepOK(a) ∧ mk' = [mk EXCEPT ![a] = @ \ {"pause"}]
                        ∧ UNCHANGED ⟨fz, rq, pc, pk⟩
          ∨ ∃ x ∈ RS : PausedRun(x) ∧ rq' = [rq EXCEPT ![x] = 1] ∧ UNCHANGED ⟨mk, fz, pc, pk⟩
          ∨ Toggle ∧ UNCHANGED ⟨mk, rq⟩
      ∨ \* status (or a commit that attaches or is cached): settling and admission only
        UNCHANGED ⟨runs, qe, mk, unit, end, rq, cxl, cancels, ui, fz, pc, pk⟩
    ∧ UNCHANGED ⟨up, crashes, lk, hk, ac, und, nxt, rec, out, ck, scr, slot, died, dec, cached, late⟩

(***************************************************************************)
(* A supervisor (`tlcq exec`) and its unit.                                *)
(***************************************************************************)
Running(a) ≜ unit[a] = "live" ∧ Ran(Cur, a)
\* Its processes run: not frozen.
Active(a) ≜ Running(a) ∧ fz[a] ∈ {"run", "thawed", "pz"}

\* The supervisor's first act.
SupStart(a) ≜
    ∧ up ∧ unit[a] = "live" ∧ "started" ∉ mk[a] ∧ ¬NoStartedMarker
    ∧ mk' = [mk EXCEPT ![a] = @ ∪ {"started"}]
    ∧ und' = [und EXCEPT ![RunOf(a)] = 0]
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, rq, nxt, cxl, rec, end, unit,
                 out, ck, scr, slot, fz, pc, pk, died, dec, cached, late⟩

Stop(a, m) ≜
    ∧ mk' = [mk EXCEPT ![a] = @ ∪ {m}]
    ∧ unit' = [unit EXCEPT ![a] = "stopping"]
    ∧ UNCHANGED ⟨late, dec⟩

\* A pause lands, under the lock and only with no cancel marker: for a running job at a completed
\* checkpoint, the paused marker, then the stop; for a frozen one unpaused for it, the same with its
\* last checkpoint, or, if it never completed one, the stop as cancelled.
SupPause(a) ≜
    ∧ up ∧ lk = "free" ∧ Active(a) ∧ "pause" ∈ mk[a] ∧ "cancel" ∉ mk[a]
    ∧ ∨ IF StopBeforeMark
        THEN ∧ unit' = [unit EXCEPT ![a] = "stopping"]
             ∧ late' = late ∪ {a} ∧ dec' = dec ∪ {a} ∧ UNCHANGED mk
        ELSE Stop(a, "paused")
      ∨ fz[a] = "pz" ∧ Stop(a, "nockpt")
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, out,
                 ck, scr, slot, fz, pc, pk, died, cached⟩

SupTimeout(a) ≜
    ∧ up ∧ lk = "free" ∧ Active(a)
    ∧ Stop(a, "timeout")
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, out,
                 ck, scr, slot, fz, pc, pk, died, cached⟩

\* StopBeforeMark: the marker, written after the stop, if the supervisor still lives.
LateMark(a) ≜
    ∧ up ∧ lk = "free" ∧ a ∈ late ∧ unit[a] = "stopping"
    ∧ mk' = [mk EXCEPT ![a] = IF "cancel" ∈ @ THEN @ ELSE @ ∪ {"paused"}]
    ∧ late' = late \ {a}
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, unit,
                 out, ck, scr, slot, fz, pc, pk, died, dec, cached⟩

\* After the stop, the supervisor moves the checkpoint to the run's checkpoint directory.
CkKeep(a) ≜
    ∧ up ∧ unit[a] = "stopping" ∧ "paused" ∈ mk[a] ∧ ¬ck[a]
    ∧ ck' = [ck EXCEPT ![a] = TRUE]
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, unit,
                 mk, out, scr, slot, fz, pc, pk, died, dec, cached, late⟩

\* The engine exits on its own (also just as a stop lands): the outcome goes into result.json.
\* A frozen engine does not run, unless a cancel unpaused it.
Exit(a) ≜
    ∧ up ∧ unit[a] ∈ {"live", "stopping"} ∧ Ran(Cur, a) ∧ ¬out[a]
    ∧ fz[a] ∈ {"run", "thawed", "pz"} ∨ unit[a] = "stopping"
    ∧ out' = [out EXCEPT ![a] = TRUE]
    ∧ unit' = [unit EXCEPT ![a] = "post"]
    ∧ late' = late \ {a}
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, mk,
                 ck, scr, slot, fz, pc, pk, died, dec, cached⟩

\* The unit dies (out of memory, a failed exec), or a requested stop completes; a paused stop
\* whose checkpoint was not yet kept dies with it.
UnitEnds(a) ≜
    ∧ up ∧ unit[a] ∈ {"live", "stopping"} ∧ (StartFails ∨ Ran(Cur, a) ∨ unit[a] = "stopping")
    ∧ unit' = [unit EXCEPT ![a] = "post"]
    ∧ died' = IF ∨ unit[a] = "live" ∧ Ran(Cur, a)
                 ∨ unit[a] = "stopping" ∧ "paused" ∈ mk[a] ∧ ¬ck[a]
              THEN died ∪ {a} ELSE died
    ∧ late' = late \ {a}
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ph, ac, runs, qe, und, rq, nxt, cxl, rec, end, mk,
                 out, ck, scr, slot, fz, pc, pk, dec, cached⟩

(***************************************************************************)
(* The stop hook: settles its attempt under the lock, step by step.        *)
(***************************************************************************)
HookTake(a) ≜
    ∧ up ∧ lk = "free" ∧ unit[a] = "post"
    ∧ lk' = "hook" ∧ hk' = {a} ∧ ph' = "cache"
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, ac, runs, qe, und, rq, nxt, cxl, rec, end, unit, mk, out,
                 ck, scr, slot, fz, pc, pk, died, dec, cached, late⟩

HookA ≜ The(hk)
Hooking(p) ≜ up ∧ lk = "hook" ∧ ph = p

\* Never started: deleted like a failed start, its job queued again (a resume at the head).
HookUndo ≜
    LET a ≜ HookA  x ≜ RunOf(a) IN
    ∧ Hooking("cache") ∧ Unrun(Cur, a)
    ∧ rec' = [rec EXCEPT ![a] = FALSE]
    ∧ slot' = [slot EXCEPT ![a] = "none"]
    ∧ mk' = [mk EXCEPT ![a] = {}]
    ∧ out' = [out EXCEPT ![a] = FALSE]
    ∧ scr' = [scr EXCEPT ![a] = FALSE]
    ∧ died' = died \ {a} ∧ dec' = dec \ {a}
    ∧ und' = [und EXCEPT ![x] = @ + 1]
    ∧ IF AtLimit(und[x])
      THEN qe' = [qe EXCEPT ![x] = "error"] ∧ rq' = [rq EXCEPT ![x] = 0]
      ELSE IF IsResume(Cur, a) THEN rq' = [rq EXCEPT ![x] = 1] ∧ UNCHANGED qe
      ELSE qe' = [qe EXCEPT ![x] = "queued"] ∧ UNCHANGED rq
    ∧ ph' = "admit"
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ac, runs, nxt, cxl, end, unit, ck, fz, pc, pk,
                 cached, late⟩

HookCache ≜
    ∧ Hooking("cache") ∧ ¬Unrun(Cur, HookA)
    ∧ cached' = IF Outcome(HookA) = "done" THEN cached ∪ {HookA[1]} ELSE cached
    ∧ ph' = "end"
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ac, runs, qe, und, rq, nxt, cxl, rec, end, unit, mk,
                 out, ck, scr, slot, fz, pc, pk, died, dec, late⟩

\* The commit point.
HookEnd ≜
    ∧ Hooking("end")
    ∧ end' = [end EXCEPT ![HookA] = Outcome(HookA)]
    ∧ ph' = "scratch"
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ac, runs, qe, und, rq, nxt, cxl, rec, unit, mk, out,
                 ck, scr, slot, fz, pc, pk, died, dec, cached, late⟩

HookScratch ≜
    ∧ Hooking("scratch")
    ∧ scr' = [scr EXCEPT ![HookA] = FALSE]
    ∧ ph' = "admit"
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, lk, hk, ac, runs, qe, und, rq, nxt, cxl, rec, end, unit, mk,
                 out, ck, slot, fz, pc, pk, died, dec, cached, late⟩

\* The hook is killed, waiting for the lock or at any step; the lock frees.
HookKill(a) ≜
    ∧ up ∧ unit[a] = "post"
    ∧ unit' = [unit EXCEPT ![a] = "absent"]
    ∧ IF lk = "hook" ∧ hk = {a}
      THEN lk' = "free" ∧ hk' = {} ∧ ph' = "none" ∧ ac' = {}
      ELSE UNCHANGED ⟨lk, hk, ph, ac⟩
    ∧ UNCHANGED ⟨up, crashes, cancels, ui, runs, qe, und, rq, nxt, cxl, rec, end, mk, out, ck, scr,
                 slot, fz, pc, pk, died, dec, cached, late⟩

(***************************************************************************)
(* Crash and boot.                                                         *)
(***************************************************************************)
Crash ≜
    ∧ up ∧ crashes < MaxCrashes
    ∧ up' = FALSE ∧ crashes' = crashes + 1
    ∧ died' = died ∪ {a ∈ Att : NoEnd(a) ∧ unit[a] ≠ "absent" ∧ Ran(Cur, a)}
    ∧ unit' = [a ∈ Att ↦ "absent"]
    ∧ lk' = "free" ∧ hk' = {} ∧ ph' = "none" ∧ ac' = {} ∧ late' = {}
    ∧ UNCHANGED ⟨cancels, ui, runs, qe, und, rq, nxt, cxl, rec, end, mk, out, ck, scr, slot, fz, pc,
                 pk, dec, cached⟩

\* `tlcq admit --boot`: settle, delete leftover scratch, admit.
Boot ≜
    ∧ ¬up
    ∧ up' = TRUE
    ∧ LET S ≜ Settled IN Apply(S)
    ∧ scr' = [a ∈ Att ↦ FALSE]
    ∧ lk' = "boot" ∧ ph' = "admit"
    ∧ UNCHANGED ⟨crashes, cancels, ui, hk, ac, runs, nxt, cxl, unit, ck, fz, pc, pk, late⟩

Quiet ≜   \* all work done: stutter rather than report a deadlock
    ∧ up ∧ lk = "free"
    \* (a run with no attempt number left within MaxAttempts is outside the model's bound)
    ∧ ∀ x ∈ RS : (qe[x] ≠ "queued" ∧ rq[x] = 0) ∨ Alloc(x) > MaxAttempts
    ∧ ∀ a ∈ Att : unit[a] = "absent" ∧ ¬NoEnd(a)
    ∧ UNCHANGED vars

Init ≜
    ∧ up = TRUE ∧ crashes = 0 ∧ cancels = 0 ∧ ui = 0
    ∧ lk = "free" ∧ hk = {} ∧ ph = "none" ∧ ac = {}
    ∧ runs = [j ∈ Ids ↦ 0]
    ∧ qe = [x ∈ RS ↦ "none"]
    ∧ und = [x ∈ RS ↦ 0]
    ∧ rq = [x ∈ RS ↦ 0]
    ∧ nxt = [x ∈ RS ↦ 0]
    ∧ cxl = [x ∈ RS ↦ Never]
    ∧ rec = [a ∈ Att ↦ FALSE]
    ∧ end = [a ∈ Att ↦ "none"]
    ∧ unit = [a ∈ Att ↦ "absent"]
    ∧ mk = [a ∈ Att ↦ {}]
    ∧ out = [a ∈ Att ↦ FALSE]
    ∧ ck = [a ∈ Att ↦ FALSE]
    ∧ scr = [a ∈ Att ↦ FALSE]
    ∧ slot = [a ∈ Att ↦ "none"]
    ∧ fz = [a ∈ Att ↦ "run"]
    ∧ pc = TRUE ∧ pk = {}
    ∧ died = {} ∧ dec = {}
    ∧ cached = {}
    ∧ late = {}

Next ≜
    ∨ VerbTake ∨ VerbApply
    ∨ AdmitRecord ∨ AdmitThaw ∨ AdmitRefreeze ∨ AdmitRun ∨ AdmitEnd
    ∨ HookUndo ∨ HookCache ∨ HookEnd ∨ HookScratch
    ∨ ∃ a ∈ Att : ∨ SupStart(a) ∨ SupPause(a) ∨ SupTimeout(a) ∨ LateMark(a) ∨ CkKeep(a)
                  ∨ Exit(a) ∨ UnitEnds(a) ∨ HookTake(a) ∨ HookKill(a)
    ∨ Crash ∨ Boot
    ∨ Quiet

Spec ≜ Init ∧ □[Next]_vars

\* Job ids are only interchangeable set members. Sound for the safety properties checked here.
\* (Whole-VM ids are interchangeable only among themselves.)
Symm ≜ Permutations(Ids \ Whole) ∪ Permutations(Whole)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)
TypeOK ≜
    ∧ up ∈ BOOLEAN ∧ crashes ∈ 0..MaxCrashes ∧ cancels ∈ 0..MaxCancels ∧ ui ∈ 0..MaxUi
    ∧ lk ∈ {"free", "verb", "boot", "hook"} ∧ hk ⊆ Att ∧ ac ⊆ Att
    ∧ ph ∈ {"apply", "cache", "end", "scratch", "admit", "run", "none"}
    ∧ runs ∈ [Ids → 0..MaxRuns]
    ∧ qe ∈ [RS → {"none", "queued", "started", "cancelled", "error"}]
    ∧ und ∈ [RS → 0..MaxAttempts]
    ∧ rq ∈ [RS → 0..1]
    ∧ nxt ∈ [RS → 0..MaxAttempts]
    ∧ cxl ∈ [RS → 0..Never]
    ∧ rec ∈ [Att → BOOLEAN] ∧ out ∈ [Att → BOOLEAN] ∧ ck ∈ [Att → BOOLEAN] ∧ scr ∈ [Att → BOOLEAN]
    ∧ end ∈ [Att → {"none"} ∪ Outcomes]
    ∧ unit ∈ [Att → {"absent", "live", "stopping", "post"}]
    ∧ mk ∈ [Att → SUBSET Markers]
    ∧ slot ∈ [Att → {"none"} ∪ Slots]
    ∧ fz ∈ [Att → Freeze]
    ∧ pc ∈ BOOLEAN ∧ pk ⊆ Att
    ∧ died ⊆ Att ∧ dec ⊆ Att ∧ late ⊆ Att
    ∧ cached ⊆ Ids

\* A run has at most one live attempt: its unit live or stopping (frozen included), or recorded
\* without an end.
OneLiveAttempt ≜
    ∀ x ∈ RS : Cardinality({a ∈ AttsOf(x) : unit[a] ∈ {"live", "stopping"} ∨ NoEnd(a)}) ≤ 1

\* Live and stopping units that hold a slot (running or thawed, not frozen) never share it, nor do
\* a slot's derived holders.
SlotsExclusive ≜
    ∀ s ∈ Slots :
        ∧ Cardinality({x ∈ RS : ∃ a ∈ AttsOf(x) : unit[a] ∈ {"live", "stopping"}
                                                  ∧ slot[a] = s ∧ fz[a] ∈ {"run", "thawed"}}) ≤ 1
        ∧ Cardinality(Occ(s)) ≤ 1

\* Nothing starts or thaws in a closed P slot: only the jobs holding it when it closed run there.
ClosedSlot ≜
    ¬pc ⇒ ∀ a ∈ Att : NoEnd(a) ∧ slot[a] = "p" ∧ fz[a] ∈ {"run", "thawed"} ⇒ a ∈ pk

\* While a whole-VM run has an attempt with no end, it alone holds a slot: E, and nothing holds P.
WholeAlone ≜
    ∀ a ∈ Att : a[1] ∈ Whole ∧ NoEnd(a) ⇒
        slot[a] ≠ "p" ∧ Occ("p") = {}

\* A pausing job is running (thawed included) or unpaused for its pause, never frozen or unfrozen.
PausingNotFrozen ≜ ∀ a ∈ Att : NoEnd(a) ∧ "pause" ∈ mk[a] ⇒ fz[a] ∈ {"run", "thawed", "pz"}

\* A paused attempt's checkpoint was kept, by it or by an earlier attempt it recovered.
PausedHasCheckpoint ≜ ∀ a ∈ Att : end[a] = "paused" ⇒ ck[a] ∨ RunKept(Cur, a)

\* A run's kept checkpoint is never lost to an error: a resume that died pauses it again.
CheckpointNotLost ≜ ∀ a ∈ Att : end[a] = "error" ⇒ ¬RunKept(Cur, a)

\* A pause stops a job only while its request stands: never once a human kept it running.
PausedAsked ≜ ∀ a ∈ Att : "paused" ∈ mk[a] ⇒ "pause" ∈ mk[a]

\* systemd-run never meets a still-loaded unit of the same name (attempt numbers are never reused).
NoNameClash ≜ ph = "run" ⇒ unit[The(ac)] = "absent"

\* No run is undone more than NeverStarted times in a row: a supervisor that cannot start does
\* not loop.
UndoLimit ≜ ∀ x ∈ RS : und[x] ≤ NeverStarted

\* A cancelled run is never queued again, never starts another attempt, and is not paused.
CancelFinal ≜
    ∀ x ∈ RS : cxl[x] ≠ Never ⇒ qe[x] ≠ "queued" ∧ rq[x] = 0 ∧ nxt[x] ≤ cxl[x] ∧ ¬PausedRun(x)

\* No run is stuck "running": a started run has ended, is paused, or has an attempt with no end
\* (live, frozen, or gone and to be settled), or a queued resume.
NoStuckRun ≜
    ∀ x ∈ RS : qe[x] = "started" ⇒
        RunOutcome(Cur, x) ≠ "none" ∨ PausedRun(x) ∨ rq[x] > 0 ∨ ∃ a ∈ AttsOf(x) : NoEnd(a)

\* Settling twice changes nothing, and settling leaves alone every run with nothing to settle.
Unsettled(a) ≜ rec[a] ∧ unit[a] = "absent" ∧ end[a] = "none"
SettleTwiceNoop ≜
    LET S ≜ Settled IN
    ∧ Settle(S) = S
    ∧ ∀ x ∈ RS : (∀ a ∈ AttsOf(x) : ¬Unsettled(a)) ⇒
                    ∧ S.q[x] = rq[x] ∧ S.u[x] = qe[x]
                    ∧ ∀ a ∈ AttsOf(x) : S.e[a] = end[a] ∧ S.r[a] = rec[a]

\* An attempt ends `error` only when it ran, then crashed or died, with nothing to say otherwise
\* (a paused marker whose checkpoint was never kept says nothing).
ErrorOnlyAfterCrash ≜
    ∀ a ∈ Att : end[a] = "error" ⇒
        ∧ a ∈ died
        ∧ "cancel" ∉ mk[a] ∧ ¬out[a] ∧ a ∉ dec
        ∧ mk[a] ∩ {"timeout", "nockpt"} = {}
        ∧ ("paused" ∈ mk[a] ⇒ ¬ck[a])

\* What a preflight reports of a run that has ended, as settling would, it keeps reporting: a
\* preflight reads only where it can (under the lock, or anywhere when PreflightUnlocked).
\* A preflight writes nothing, so this is the check of every preflight at every point.
Reports ≜ LET S ≜ Settled IN [x ∈ RS ↦ RunOutcome(S, x)]
PreflightTrue ≜
    [][LET r ≜ Reports  r2 ≜ Reports' IN
       (lk = "free" ∨ PreflightUnlocked) ⇒ ∀ x ∈ RS : r[x] ∈ Ending ⇒ r2[x] = r[x]]_vars

\* A paused run stays paused, crashes and reboots included, until a resume or a cancel.
PausedStays ≜
    [][∀ x ∈ RS : PausedRun(x) ⇒ PausedRun(x)' ∨ rq'[x] > 0 ∨ (RunOutcome(Cur, x))' = "cancelled"]_vars
=============================================================================
