----------------------------- MODULE Placement -----------------------------
(***************************************************************************)
(* tlcq's record protocol between the laptop and the VM (openspec change   *)
(* tlc-queue, design D6, D7, D10, D14). Commands place jobs one at a time  *)
(* under the laptop's placement lock: preflight (refresh the laptop's      *)
(* records and mirror, attach, refuse, choose a route), laptop supersedes  *)
(* and the write-ahead `sent?` record, one commit to the VM whose reply    *)
(* can be lost after it applied or before, then the records. Laptop jobs   *)
(* run under supervisors that hold a job lock (a flock: freed when its     *)
(* holder dies), take the one laptop slot, may time out and move to the    *)
(* VM, and may crash. `cancel` kills a laptop supervisor and ends the job  *)
(* itself. The VM queues, runs and stops units on its own, and may become  *)
(* unreachable. Every job is one configuration; each submitting worktree  *)
(* holds an entry on it (laptop, VM, or pending while offline), so two     *)
(* ids sharing a worktree's entry are two versions of its configuration.   *)
(*                                                                         *)
(* Left out (design D14): the generation counter (polls apply atomically); *)
(* the agents of a commit whose reply is lost, ended at once here (the     *)
(* design has their follows wait for a refresh to resolve the token);      *)
(* when a token becomes final (commits apply atomically here, so no       *)
(* lookup races one);                                                      *)
(* an `invalid` answer (nothing applied; the command undoes its `sent?`    *)
(* writes, leaving its records as before the commit, less the superseded  *)
(* laptop runs and the worktree's pending entries dropped from superseded *)
(* jobs); a commit answered `cached` (the record ends, as the next poll   *)
(* would end it);                                                          *)
(* a `now-or-refuse` job the VM refuses (its `sent?` record becomes a      *)
(* laptop record; safe, the VM never stored it); slot shares, caps, lane   *)
(* decisions and restarts (a restart continues the run, like a move);      *)
(* cache contents beyond "cached or not"; batches (one job per command);   *)
(* more than one configuration (an entry is its worktree); the expectation *)
(* and route, chosen freely among the allowed ones.                        *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Ids,            \* job ids: versions of one configuration
          Cmds,           \* commands (`tlcq submit`), each placing one job, whose agent then follows it
          Wts,            \* worktrees
          NoOne,
          MaxOutages,     \* how often the VM may become unreachable
          MaxCancels,     \* how many `cancel`s may run
          HandoffGap,     \* bug: the command frees the job lock before the supervisor takes it
          AttachByRecord, \* bug: attach to a VM job only when the laptop has a record of it
          StaleOffline,   \* bug: while the VM is unreachable, an unresolved record counts as gone
          NoWriteAhead,   \* bug: a job is recorded `sent?` only once its commit's reply is lost
          SupersedeAfterCommit, \* bug: laptop supersedes wait for the commit's answer
          NoPendingInScan, \* bug: the older-version scan ignores pending entries
          SupersedeAll,   \* bug: a supersede cancels a job whatever other entries it has
          FlushLate,      \* bug: a commit flushes pending entries after its supersedes
          NoOwnEntry,     \* bug: the write-ahead record does not carry the command's own entry
          AttachExempt,   \* bug: an attach skips the older-version rule
          KeepPending,    \* bug: a supersede keeps the worktree's pending entries on the job
          SentByToken     \* bug: a `sent?` record is refreshed by its commit's token, not by the id

VARIABLES
    up, outages,  \* the VM is reachable; outages so far
    pl,           \* the placement lock: NoOne, a command, or an id (its supervisor moving it)
    jl,           \* [Ids -> NoOne | "sup"]: the laptop job lock
    rec,          \* [Ids -> "none" | "laptop" | "moving" | "vm" | "sentq" | "ended"]: laptop record
    sup,          \* [Ids -> "none" | "waiting" | "running" | "timedout" | "moving" | "dead" | "done"]
    mark,         \* [Ids -> "none" | "cancel" | "timeout"]: the laptop run's marker
    vm,           \* [Ids -> "none" | "queued" | "running" | "stopping" | "ended"]; a new run
                  \* of a stopping job does not wait for it (its own unit, D10)
    runs,         \* [Ids -> Nat]: the job's current run; a run placed anew counts up
    lent,         \* [Ids -> SUBSET Wts]: entries of a laptop job
    vent,         \* [Ids -> SUBSET Wts]: entries of a VM job
    ment,         \* [Ids -> SUBSET Wts]: the laptop's mirror of vent
    pend,         \* [Ids -> SUBSET (Wts \X Nat)]: pending entries, attached while the VM was
                  \* unreachable, each naming the run it attached to
    srun,         \* [Ids -> Nat]: the run the last `sent?` commit reached, as its token
                  \* resolves (0: it reached none)
    cached,       \* ids with a cached result, on either machine
    victims,      \* commands of another worktree waiting on a job a supersede cancelled (ghost)
    cmd,          \* [Cmds -> [pc, id, wt, rep, route, old, run]]
    cancels

vars ≜ ⟨up, outages, pl, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun,
        cached, victims, cmd, cancels⟩

Alive ≜ {"waiting", "running", "timedout", "moving"}
\* Routes: "vm" / "laptop" place anew; "avm" / "alap" / "aoff" attach (VM, laptop, offline).
ToVM ≜ {"vm", "avm"}
None ≜ [j ∈ Ids ↦ {}]

Init ≜
    ∧ up = TRUE ∧ outages = 0
    ∧ pl = NoOne
    ∧ jl = [j ∈ Ids ↦ NoOne]
    ∧ rec = [j ∈ Ids ↦ "none"]
    ∧ sup = [j ∈ Ids ↦ "none"]
    ∧ mark = [j ∈ Ids ↦ "none"]
    ∧ vm = [j ∈ Ids ↦ "none"]
    ∧ runs = [j ∈ Ids ↦ 0]
    ∧ lent = None ∧ vent = None ∧ ment = None ∧ pend = None
    ∧ srun = [j ∈ Ids ↦ 0]
    ∧ cached = {}
    ∧ victims = {}
    ∧ cmd = [c ∈ Cmds ↦ [pc ↦ "idle", id ↦ NoOne, wt ↦ NoOne, rep ↦ FALSE,
                          route ↦ "none", old ↦ {}, run ↦ 0]]
    ∧ cancels = 0

(***************************************************************************)
(* The refresh a preflight or a poll applies to the laptop's records.      *)
(***************************************************************************)
Unresolved(j) ≜ rec[j] ∈ {"vm", "sentq"} ∨ (rec[j] = "moving" ∧ jl[j] = NoOne)

Refreshed(j) ≜
    CASE rec[j] = "laptop" ∧ jl[j] = NoOne → "ended"           \* an orphan
      □ Unresolved(j) ∧ ¬up → IF StaleOffline THEN "ended" ELSE rec[j]
      □ rec[j] = "sentq" ∧ SentByToken →
             IF srun[j] ≠ 0 ∧ srun[j] = runs[j] ∧ vm[j] ∈ {"queued", "running", "stopping"}
             THEN "vm" ELSE "ended"
      □ Unresolved(j) → IF vm[j] ∈ {"queued", "running", "stopping"} THEN "vm" ELSE "ended"
      □ OTHER → rec[j]

Refresh ≜ [j ∈ Ids ↦ Refreshed(j)]
Mirror ≜ IF up THEN vent ELSE ment

\* The VM may hold j (as the laptop sees it after the refresh R).
OnVM(R, j) ≜ IF up THEN vm[j] ∈ {"queued", "running"} ELSE R[j] ∈ {"vm", "sentq", "moving"}
LiveVM(R, j) ≜ IF AttachByRecord THEN OnVM(R, j) ∧ R[j] = "vm" ELSE OnVM(R, j)
LiveLaptop(j) ≜ jl[j] = "sup"

\* The worktrees of a job's pending entries: all of them, or those naming its current run.
PendW(j) ≜ {p[1] : p ∈ pend[j]}
PendOn(j) ≜ {p[1] : p ∈ {q ∈ pend[j] : q[2] = runs[j]}}

\* The worktrees a command sees on a job: laptop entries, the VM's (or the mirror's), pending.
Seen(o) ≜ lent[o] ∪ Mirror[o] ∪ (IF NoPendingInScan THEN {} ELSE PendW(o))

RunningOld(R, o) ≜ sup[o] ∈ {"running", "timedout", "moving"} ∨ (up ∧ vm[o] = "running")
                   ∨ (¬up ∧ R[o] ∈ {"vm", "sentq", "moving"})
QueuedOld(o) ≜ sup[o] = "waiting" ∨ (up ∧ vm[o] = "queued")
SlotFree ≜ ∀ k ∈ Ids : sup[k] ∉ {"waiting", "running"}

\* Commands of another worktree than w waiting on a job in `dead`.
Wronged(dead, w) ≜ {c ∈ Cmds : cmd[c].pc = "follow" ∧ cmd[c].id ∈ dead ∧ cmd[c].wt ≠ w
                                ∧ cmd[c].run = runs[cmd[c].id]}

SetCmd(c, f, v) ≜ cmd' = [cmd EXCEPT ![c][f] = v]
Finish(c) ≜ ∧ cmd' = [cmd EXCEPT ![c].pc = "done"] ∧ pl' = NoOne

(***************************************************************************)
(* A command.                                                              *)
(***************************************************************************)
Start(c) ≜
    ∧ cmd[c].pc = "idle"
    ∧ ∃ i ∈ Ids, w ∈ Wts, r ∈ BOOLEAN :
        cmd' = [cmd EXCEPT ![c] = [pc ↦ "lock", id ↦ i, wt ↦ w, rep ↦ r,
                                    route ↦ "none", old ↦ {}, run ↦ 0]]
    ∧ UNCHANGED ⟨up, outages, pl, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun,
                 cached, victims, cancels⟩

Lock(c) ≜
    ∧ cmd[c].pc = "lock" ∧ pl = NoOne
    ∧ pl' = c
    ∧ SetCmd(c, "pc", "pre")
    ∧ UNCHANGED ⟨up, outages, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun,
                 cached, victims, cancels⟩

\* A preflight that connects, then fails: exit 3, nothing written.
PreflightCut(c) ≜
    ∧ cmd[c].pc = "pre" ∧ pl = c ∧ up
    ∧ Finish(c)
    ∧ UNCHANGED ⟨up, outages, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun,
                 cached, victims, cancels⟩

Preflight(c) ≜
    LET i ≜ cmd[c].id  w ≜ cmd[c].wt
        R ≜ Refresh
        olds ≜ {o ∈ Ids \ {i} : w ∈ Seen(o)}
        running ≜ {o ∈ olds : RunningOld(R, o)}
        queued ≜ {o ∈ olds : QueuedOld(o)}
        attachVM ≜ ¬LiveLaptop(i) ∧ LiveVM(R, i)
        attach ≜ LiveLaptop(i) ∨ attachVM
        \* An attach counts as a run for the older-version rule; only a cache hit is exempt.
        exempt ≜ AttachExempt ∧ attach
        routes ≜ IF LiveLaptop(i) THEN {"alap"}
                 ELSE IF attachVM THEN (IF up THEN {"avm"} ELSE {"aoff"})
                 ELSE IF up THEN {"vm"} ∪ (IF SlotFree THEN {"laptop"} ELSE {})
                 ELSE {"laptop", "refuse"}
    IN
    ∧ cmd[c].pc = "pre" ∧ pl = c
    ∧ ment' = Mirror
    ∧ rec' = R
    ∧ CASE i ∈ cached →                                         \* a cache hit
             Finish(c)
        □ running ≠ {} ∧ ¬cmd[c].rep ∧ ¬exempt →                 \* an older version runs
             Finish(c)
        □ ¬up ∧ cmd[c].rep ∧ ¬exempt ∧ ∃ o ∈ olds : R[o] ∈ {"vm", "sentq", "moving"} →
             Finish(c)                                           \* cannot reach it
        □ OTHER →
             ∃ route ∈ routes :
                 IF route = "refuse"                             \* a long job offline
                 THEN Finish(c)
                 ELSE ∧ cmd' = [cmd EXCEPT ![c].pc = IF up THEN "kill" ELSE "killoff",
                                           ![c].route = route,
                                           ![c].old = IF exempt THEN {}
                                                      ELSE queued ∪ (IF cmd[c].rep THEN running ELSE {})]
                      ∧ pl' = c
    ∧ UNCHANGED ⟨lent, vent, pend, srun⟩
    ∧ UNCHANGED ⟨up, outages, jl, sup, mark, vm, runs, cached, victims, cancels⟩

\* Supersede on the laptop: drop w's entry; a job left with no entry is killed.
LaptopOld(c) ≜ {o ∈ cmd[c].old : rec[o] = "laptop"}

Kill(c) ≜
    LET w ≜ cmd[c].wt
        lold ≜ IF SupersedeAfterCommit THEN {} ELSE LaptopOld(c)
        left(o) ≜ lent[o] \ {w}
        dead ≜ {o ∈ lold : left(o) = {} ∨ SupersedeAll}
    IN
    ∧ cmd[c].pc ∈ {"kill", "killoff"} ∧ pl = c
    ∧ lent' = [j ∈ Ids ↦ IF j ∈ lold THEN left(j) ELSE lent[j]]
    ∧ rec' = [j ∈ Ids ↦ IF j ∈ dead THEN "ended" ELSE rec[j]]
    ∧ sup' = [j ∈ Ids ↦ IF j ∈ dead ∧ sup[j] ∈ Alive THEN "dead" ELSE sup[j]]
    ∧ mark' = [j ∈ Ids ↦ IF j ∈ dead THEN "cancel" ELSE mark[j]]
    ∧ jl' = [j ∈ Ids ↦ IF j ∈ dead THEN NoOne ELSE jl[j]]
    ∧ victims' = victims ∪ Wronged(dead, w)
    \* Superseding a job also drops w's pending entries on it, before the commit, so a commit
    \* whose reply is lost cannot flush them again later.
    ∧ pend' = IF KeepPending THEN pend
              ELSE [j ∈ Ids ↦ IF j ∈ cmd[c].old THEN {p ∈ pend[j] : p[1] ≠ w} ELSE pend[j]]
    ∧ SetCmd(c, "pc", IF cmd[c].pc = "kill" THEN "commit" ELSE "records")
    ∧ UNCHANGED ⟨up, outages, pl, vm, runs, vent, ment, srun, cached, cancels⟩

\* The VM's side of a commit from worktree w: pending entries (each onto its own run only),
\* the job, supersedes.
Flush(e) ≜ [j ∈ Ids ↦ IF vm[j] ∈ {"queued", "running"} THEN e[j] ∪ PendOn(j) ELSE e[j]]

VmApply(i, w, route, old) ≜
    LET e0 ≜ IF FlushLate THEN vent ELSE Flush(vent)
        fresh ≜ route ∈ ToVM ∧ vm[i] ∈ {"none", "ended", "stopping"}
        e1 ≜ IF route ∈ ToVM
             THEN [e0 EXCEPT ![i] = IF fresh THEN {w} ELSE @ ∪ {w}] ELSE e0
        vold ≜ {o ∈ old : vm[o] ∈ {"queued", "running"}}
        e2 ≜ [j ∈ Ids ↦ IF j ∈ vold THEN e1[j] \ {w} ELSE e1[j]]
        dead ≜ {o ∈ vold : e2[o] = {} ∨ SupersedeAll}
        e3 ≜ IF FlushLate THEN [j ∈ Ids ↦ IF vm[j] ∈ {"queued", "running"} ∧ j ∉ dead
                                         THEN e2[j] ∪ PendOn(j) ELSE e2[j]]
             ELSE e2
    IN ∧ vm' = [j ∈ Ids ↦
                  IF j = i ∧ fresh THEN "queued"
                  ELSE IF j ∈ dead ∧ vm[j] = "queued" THEN "ended"
                  ELSE IF j ∈ dead ∧ vm[j] = "running" THEN "stopping"
                  ELSE vm[j]]
       ∧ vent' = e3
       ∧ runs' = [runs EXCEPT ![i] = IF fresh THEN @ + 1 ELSE @]
       ∧ victims' = victims ∪ Wronged(dead, w)

Commit(c) ≜
    LET i ≜ cmd[c].id  route ≜ cmd[c].route  old ≜ cmd[c].old
        sent ≜ IF route ∈ ToVM THEN [rec EXCEPT ![i] = "sentq"] ELSE rec
        \* the commit's token, kept by the VM on the run the job reached
        reached ≜ IF route ∈ ToVM THEN [srun EXCEPT ![i] = runs'[i]] ELSE srun
    IN
    ∧ cmd[c].pc = "commit" ∧ pl = c
    \* The write-ahead record carries the command's own entry, in the mirror.
    ∧ ment' = IF route ∈ ToVM ∧ ¬NoOwnEntry THEN [ment EXCEPT ![i] = @ ∪ {cmd[c].wt}] ELSE ment
    ∧ ∨ \* answered: the records step clears the flushed pending entries
        ∧ up
        ∧ VmApply(i, cmd[c].wt, route, old)
        ∧ pend' = None
        ∧ rec' = IF NoWriteAhead THEN rec ELSE sent
        ∧ srun' = reached
        ∧ SetCmd(c, "pc", "records")
        ∧ pl' = c
      ∨ \* applied, the reply lost: pending entries kept (flushing again is harmless)
        ∧ up
        ∧ VmApply(i, cmd[c].wt, route, old)
        ∧ rec' = sent
        ∧ srun' = reached
        ∧ Finish(c)
        ∧ UNCHANGED pend
      ∨ \* lost before it applied (a failure to connect included)
        ∧ rec' = sent
        ∧ Finish(c)
        ∧ srun' = IF route ∈ ToVM THEN [srun EXCEPT ![i] = 0] ELSE srun
        ∧ UNCHANGED ⟨vm, runs, vent, pend, victims⟩
    ∧ UNCHANGED ⟨up, outages, jl, sup, mark, lent, cached, cancels⟩

Records(c) ≜
    LET i ≜ cmd[c].id  w ≜ cmd[c].wt  route ≜ cmd[c].route
        lold ≜ IF SupersedeAfterCommit THEN LaptopOld(c) ELSE {}
        dead ≜ {o ∈ lold : lent[o] \ {w} = {} ∨ SupersedeAll}
        \* The run the laptop knows: its record's, or for `sent?` the run its token reached.
        \* Offline, only a commit from this laptop starts a run, and it writes `sent?` first.
        known ≜ IF rec[i] = "sentq" THEN srun[i] ELSE runs[i]
    IN
    ∧ cmd[c].pc = "records" ∧ pl = c
    ∧ route = "laptop" ⇒ jl[i] = NoOne
    ∧ rec' = [j ∈ Ids ↦ IF j = i ∧ route = "laptop" THEN "laptop"
                        ELSE IF j = i ∧ route ∈ ToVM THEN "vm"
                        ELSE IF j ∈ dead THEN "ended" ELSE rec[j]]
    ∧ lent' = [j ∈ Ids ↦ IF j = i ∧ route = "laptop" THEN {w}
                         ELSE IF j = i ∧ route = "alap" THEN lent[j] ∪ {w}
                         ELSE IF j ∈ lold THEN lent[j] \ {w} ELSE lent[j]]
    ∧ pend' = IF route = "aoff" THEN [pend EXCEPT ![i] = @ ∪ {⟨w, known⟩}] ELSE pend
    ∧ sup' = [j ∈ Ids ↦ IF j = i ∧ route = "laptop" THEN "waiting"
                        ELSE IF j ∈ dead ∧ sup[j] ∈ Alive THEN "dead" ELSE sup[j]]
    ∧ mark' = [j ∈ Ids ↦ IF j = i ∧ route = "laptop" THEN "none"
                         ELSE IF j ∈ dead THEN "cancel" ELSE mark[j]]
    ∧ jl' = [j ∈ Ids ↦ IF j = i ∧ route = "laptop" THEN (IF HandoffGap THEN NoOne ELSE "sup")
                       ELSE IF j ∈ dead THEN NoOne ELSE jl[j]]
    ∧ victims' = victims ∪ Wronged(dead, w)
    ∧ runs' = [runs EXCEPT ![i] = IF route = "laptop" THEN @ + 1 ELSE @]
    \* the agent's reports follow this run, not the id: they end when the run ends, even if the id is placed anew.
    ∧ cmd' = [cmd EXCEPT ![c].pc = "follow",
                         ![c].run = IF route = "aoff" THEN known ELSE runs'[i]]
    ∧ pl' = NoOne
    ∧ UNCHANGED ⟨up, outages, vm, vent, ment, srun, cached, cancels⟩

\* A command killed while placing: its flocks are freed, nothing more is written.
CmdCrash(c) ≜
    ∧ cmd[c].pc ∈ {"pre", "kill", "killoff", "commit", "records"}
    ∧ Finish(c)
    ∧ UNCHANGED ⟨up, outages, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun,
                 cached, victims, cancels⟩

Done(c) ≜
    ∧ cmd[c].pc = "follow"
    ∧ rec[cmd[c].id] = "ended" ∨ cmd[c].id ∈ cached ∨ runs[cmd[c].id] ≠ cmd[c].run
    ∧ cmd' = [cmd EXCEPT ![c].pc = "done"]
    ∧ UNCHANGED ⟨up, outages, pl, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun,
                 cached, victims, cancels⟩

\* `status`: a poll, applied under the placement lock.
Poll ≜
    ∧ pl = NoOne ∧ up
    ∧ rec' = Refresh ∧ ment' = vent
    ∧ ⟨rec', ment'⟩ ≠ ⟨rec, ment⟩
    ∧ UNCHANGED ⟨up, outages, pl, jl, sup, mark, vm, runs, lent, vent, pend, srun,
                 cached, victims, cmd, cancels⟩

(***************************************************************************)
(* A laptop supervisor.                                                    *)
(***************************************************************************)
Rest ≜ ⟨up, outages, runs, lent, vent, ment, pend, srun, victims, cmd, cancels⟩

SupTake(j) ≜     \* only reachable with HandoffGap
    ∧ sup[j] = "waiting" ∧ jl[j] = NoOne
    ∧ jl' = [jl EXCEPT ![j] = "sup"]
    ∧ UNCHANGED ⟨pl, rec, sup, mark, vm, cached⟩ ∧ UNCHANGED Rest

SupStart(j) ≜
    ∧ sup[j] = "waiting" ∧ jl[j] = "sup"
    ∧ ∀ k ∈ Ids : sup[k] ≠ "running"
    ∧ sup' = [sup EXCEPT ![j] = "running"]
    ∧ UNCHANGED ⟨pl, jl, rec, mark, vm, cached⟩ ∧ UNCHANGED Rest

SupEnd(j) ≜      \* placement lock, end state, job lock freed, placement lock freed
    ∧ sup[j] ∈ {"running", "timedout"} ∧ pl = NoOne
    ∧ rec' = [rec EXCEPT ![j] = "ended"]
    ∧ cached' = IF sup[j] = "running" THEN cached ∪ {j} ELSE cached
    ∧ sup' = [sup EXCEPT ![j] = "done"]
    ∧ jl' = [jl EXCEPT ![j] = NoOne]
    ∧ UNCHANGED ⟨pl, mark, vm⟩ ∧ UNCHANGED Rest

SupTimeout(j) ≜
    ∧ sup[j] = "running"
    ∧ sup' = [sup EXCEPT ![j] = "timedout"]
    ∧ mark' = [mark EXCEPT ![j] = "timeout"]
    ∧ UNCHANGED ⟨pl, jl, rec, vm, cached⟩ ∧ UNCHANGED Rest

SupMove(j) ≜     \* --vm-on-timeout: placement lock, then `moving`
    ∧ sup[j] = "timedout" ∧ pl = NoOne
    ∧ pl' = j
    ∧ rec' = [rec EXCEPT ![j] = "moving"]
    ∧ sup' = [sup EXCEPT ![j] = "moving"]
    ∧ UNCHANGED ⟨jl, mark, vm, cached⟩ ∧ UNCHANGED Rest

\* The move commits the job with every laptop entry.
MoveApply(j) ≜
    LET fresh ≜ vm[j] ∈ {"none", "ended", "stopping"} IN
    ∧ vm' = [vm EXCEPT ![j] = IF fresh THEN "queued" ELSE @]
    ∧ vent' = [vent EXCEPT ![j] = IF fresh THEN lent[j] ELSE @ ∪ lent[j]]
    ∧ UNCHANGED runs                                            \* the same run, moved

SupMoveCommit(j) ≜
    ∧ sup[j] = "moving" ∧ pl = j
    ∧ ∨ ∧ up ∧ MoveApply(j) ∧ rec' = [rec EXCEPT ![j] = "vm"]   \* answered
      ∨ ∧ up ∧ MoveApply(j) ∧ UNCHANGED rec                      \* applied, reply lost
      ∨ UNCHANGED ⟨vm, vent, runs, rec⟩                         \* lost before
    ∧ sup' = [sup EXCEPT ![j] = "done"]
    ∧ jl' = [jl EXCEPT ![j] = NoOne]
    ∧ pl' = NoOne
    ∧ UNCHANGED ⟨up, outages, mark, lent, ment, pend, srun, cached, victims, cmd, cancels⟩

SupCrash(j) ≜
    ∧ sup[j] ∈ Alive
    ∧ sup' = [sup EXCEPT ![j] = "dead"]
    ∧ jl' = [jl EXCEPT ![j] = IF @ = "sup" THEN NoOne ELSE @]
    ∧ pl' = IF pl = j THEN NoOne ELSE pl
    ∧ UNCHANGED ⟨rec, mark, vm, cached⟩ ∧ UNCHANGED Rest

(***************************************************************************)
(* `cancel` (ends the job for everyone) and the VM.                        *)
(***************************************************************************)
CancelLaptop(j) ≜
    ∧ cancels < MaxCancels ∧ pl = NoOne ∧ rec[j] = "laptop"
    ∧ sup' = [sup EXCEPT ![j] = IF @ ∈ Alive THEN "dead" ELSE @]
    ∧ mark' = [mark EXCEPT ![j] = "cancel"]
    ∧ jl' = [jl EXCEPT ![j] = NoOne]
    ∧ rec' = [rec EXCEPT ![j] = "ended"]
    ∧ cancels' = cancels + 1
    ∧ UNCHANGED ⟨up, outages, pl, vm, runs, lent, vent, ment, pend, srun, cached, victims, cmd⟩

CancelVM(j) ≜
    ∧ cancels < MaxCancels ∧ up ∧ vm[j] ∈ {"queued", "running"}
    ∧ vm' = [vm EXCEPT ![j] = IF @ = "queued" THEN "ended" ELSE "stopping"]
    ∧ cancels' = cancels + 1
    ∧ UNCHANGED ⟨up, outages, pl, jl, rec, sup, mark, runs, lent, vent, ment, pend, srun,
                 cached, victims, cmd⟩

VmRest ≜ ⟨up, outages, pl, jl, rec, sup, mark, lent, vent, ment, pend, srun, victims, cmd, cancels⟩

VmStart(j) ≜
    ∧ vm[j] = "queued"
    ∧ vm' = [vm EXCEPT ![j] = "running"]
    ∧ UNCHANGED ⟨runs, cached⟩ ∧ UNCHANGED VmRest

VmEnd(j) ≜
    ∧ vm[j] = "running"
    ∧ vm' = [vm EXCEPT ![j] = "ended"]
    ∧ ∃ ok ∈ BOOLEAN : cached' = IF ok THEN cached ∪ {j} ELSE cached
    ∧ UNCHANGED ⟨runs⟩ ∧ UNCHANGED VmRest

VmStopped(j) ≜
    ∧ vm[j] = "stopping"
    ∧ vm' = [vm EXCEPT ![j] = "ended"]
    ∧ UNCHANGED runs
    ∧ UNCHANGED cached ∧ UNCHANGED VmRest

Outage ≜
    ∧ up ∧ outages < MaxOutages
    ∧ up' = FALSE ∧ outages' = outages + 1
    ∧ UNCHANGED ⟨pl, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun, cached, victims,
                 cmd, cancels⟩

Recover ≜
    ∧ ¬up ∧ up' = TRUE
    ∧ UNCHANGED ⟨outages, pl, jl, rec, sup, mark, vm, runs, lent, vent, ment, pend, srun, cached,
                 victims, cmd, cancels⟩

Quiet ≜   \* every command finished: stutter rather than report a deadlock
    ∧ ∀ c ∈ Cmds : cmd[c].pc = "done"
    ∧ UNCHANGED vars

Next ≜
    ∨ ∃ c ∈ Cmds : Start(c) ∨ Lock(c) ∨ PreflightCut(c) ∨ Preflight(c)
                   ∨ Kill(c) ∨ Commit(c) ∨ Records(c) ∨ CmdCrash(c) ∨ Done(c)
    ∨ Poll
    ∨ ∃ j ∈ Ids : SupTake(j) ∨ SupStart(j) ∨ SupEnd(j) ∨ SupTimeout(j)
                  ∨ SupMove(j) ∨ SupMoveCommit(j) ∨ SupCrash(j)
                  ∨ CancelLaptop(j) ∨ CancelVM(j)
                  ∨ VmStart(j) ∨ VmEnd(j) ∨ VmStopped(j)
    ∨ Outage ∨ Recover
    ∨ Quiet

Spec ≜ Init ∧ □[Next]_vars

\* Commands, worktrees and job ids are each used only as interchangeable set members, so TLC
\* may treat states that differ by renaming them as one. Sound for the safety properties
\* checked here (not for liveness).
Symm ≜ Permutations(Cmds) ∪ Permutations(Wts) ∪ Permutations(Ids)

(***************************************************************************)
(* Properties.                                                             *)
(***************************************************************************)
TypeOK ≜
    ∧ up ∈ BOOLEAN ∧ outages ∈ 0..MaxOutages ∧ cancels ∈ 0..MaxCancels
    ∧ pl ∈ {NoOne} ∪ Cmds ∪ Ids
    ∧ jl ∈ [Ids → {NoOne, "sup"}]
    ∧ rec ∈ [Ids → {"none", "laptop", "moving", "vm", "sentq", "ended"}]
    ∧ sup ∈ [Ids → {"none", "dead", "done"} ∪ Alive]
    ∧ mark ∈ [Ids → {"none", "cancel", "timeout"}]
    ∧ vm ∈ [Ids → {"none", "queued", "running", "stopping", "ended"}]
    ∧ runs ∈ [Ids → Nat]
    ∧ ∀ e ∈ {lent, vent, ment} : e ∈ [Ids → SUBSET Wts]
    ∧ ∀ j ∈ Ids : pend[j] ⊆ Wts × Nat
    ∧ srun ∈ [Ids → Nat]
    ∧ cached ⊆ Ids
    ∧ victims ⊆ Cmds

\* A job is never on the laptop (waiting or running) and on the VM (queued or running) at once.
NoTwoPlaces ≜ ∀ j ∈ Ids : ¬(sup[j] ∈ {"waiting", "running"} ∧ vm[j] ∈ {"queued", "running"})

\* A laptop record whose job lock is free (an orphan to whoever finds it) has no live supervisor.
OrphanSound ≜ ∀ j ∈ Ids : rec[j] = "laptop" ∧ jl[j] = NoOne ⇒ sup[j] ∉ Alive

\* No worktree has two versions running at once.
\* The worktrees a live run belongs to (an ended run's entries are history).
Own(x) ≜ (IF sup[x] ∈ Alive THEN lent[x] ELSE {})
         ∪ (IF vm[x] ∈ {"queued", "running", "stopping"} THEN vent[x] ∪ PendOn(x) ELSE {})
Running(x) ≜ sup[x] = "running" ∨ vm[x] = "running"
OneVersion ≜ ∀ w ∈ Wts, x, y ∈ Ids :
                 x ≠ y ∧ Running(x) ∧ Running(y) ⇒ ¬(w ∈ Own(x) ∧ w ∈ Own(y))

\* One worktree's supersede never cancels a job another worktree already waits for. (One that
\* attaches offline to a job already cancelled reads the cancel when the VM is back.)
WaitersRespected ≜ victims = {}
=============================================================================
