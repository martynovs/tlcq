## MODIFIED Requirements

### Requirement: A web page shows the queue
The runner SHALL serve a web page, routed by pve's traefik at `tlcq.pve.local`, with no login. It SHALL show the E and P slots, whether the P slot is open, and what holds each (a job, or nothing); the queue in its order, with each run's position; the frozen and paused runs, each with since when (a paused run with when it stopped at its checkpoint and its counters then (a frozen job paused keeps its last checkpoint, which may be older), a frozen one with whether it runs on spare capacity now); whether the queue is held; for each running job its progress (TLC's counters and their rates, the queue as a fraction of the distinct states and its growth over the last 5 minutes, the probe's numbers, its lane and the source of it, its time cap and time left, and its estimated end time with the source of the estimate); for each ended job its outcome (with the canceller's label and any "replaced by" id, or an error's reason), TLC's exit status, duration and logs; and for every job its id, configuration name and submitters (worktree and label). The page SHALL refresh itself from the runner's state every few seconds. The estimated end time SHALL use the estimators of the job-status spec on the curves of the runs the runner holds.

#### Scenario: Watching a long run
- **WHEN** a human opens the page while a max job runs in the E slot and two jobs wait
- **THEN** it shows the max job's counters, growth, time left and estimated end, and the two waiting jobs in their order

#### Scenario: An ended job's logs
- **WHEN** a human opens an ended job on the page
- **THEN** it shows the outcome, exit status and duration, and TLC's log

### Requirement: A human can reorder the queue
The page SHALL let a human move a queued run up, down, or to the head of the queue. A reorder SHALL change only the queue's order, under the admission lock; it SHALL NOT move a run that has a recorded attempt, or a resumed or unfrozen run, which keeps its place at the head, and new runs SHALL still join at the tail.

#### Scenario: Moving a job to the head
- **WHEN** slow jobs A, B and C wait in that order behind a running job, and a human moves C to the head
- **THEN** C starts when the running job ends, then A, then B

#### Scenario: A resumed run stays first
- **WHEN** a paused run was resumed and waits for a slot, and a human moves another run to the head
- **THEN** the resumed run still starts first, and the moved run is next

## ADDED Requirements

### Requirement: A human can freeze and unfreeze a job
The page SHALL offer freeze on every running or thawed VM job that is not pausing, and unfreeze on every frozen or thawed one that is not pausing, each a verb under the admission lock. A freeze SHALL pause the job's container (no CPU, the memory it uses kept, the rest of its limit no longer promised) and free its slot at once. An unfreeze SHALL make the job an ordinary run again: thawed, it keeps running in its slot and is no longer frozen again for new work; frozen, it goes first in the queue and thaws once a slot is free with its limit within that slot's share and room left for it to grow. The page SHALL show a frozen job that memory keeps frozen, and a waiting run that frozen jobs' memory holds. Frozen time SHALL count toward no lane timer, cap or duration.

#### Scenario: Making room for a short job
- **WHEN** two max jobs hold both slots, a short job waits, and a human freezes one max job
- **THEN** the short job starts in the freed slot at once, and the frozen job keeps its memory and its progress

#### Scenario: Spare capacity
- **WHEN** a job is frozen, a slot is free and nothing waits
- **THEN** the frozen job runs in that slot, and is frozen again when a new job arrives that needs the slot

#### Scenario: Unfreeze
- **WHEN** a human unfreezes a frozen job while both slots are busy and other jobs wait
- **THEN** it takes the next free slot before them, and is not frozen again for new work

### Requirement: A human can pause and resume a job
The page SHALL offer pause on every running, thawed or frozen VM job, and resume on every paused one, each a verb under the admission lock. A pause of a running or thawed job SHALL stop it just after the engine's next completed checkpoint and keep that checkpoint; of a frozen job, unpause it, let a checkpoint being written complete, stop it, and keep the last checkpoint completed, or end it `cancelled` ("paused before its first checkpoint") if it has none. Once the engine's search has ended (its liveness check runs), the page SHALL offer no pause and the verb SHALL refuse one, since no checkpoint is coming and tlx has deleted its last; a pause requested just before SHALL wait for the run's end, and the page SHALL say no checkpoint is coming. Until a pause lands, the page SHALL show the job pausing. A paused run SHALL hold no slot and no memory and SHALL survive a runner restart and a reboot. A resume SHALL put the run first in the queue, and its next attempt SHALL recover the checkpoint in whichever slot is free.

#### Scenario: Before the nightly reboot
- **WHEN** a human pauses a max job in the evening, and pve reboots in the night
- **THEN** the job stops at its next checkpoint, frees its slot, and is still paused after the reboot

#### Scenario: Resuming in the morning
- **WHEN** the human resumes it in the morning while both slots are busy
- **THEN** it takes the next free slot before the queue, recovers its checkpoint, and its counters go on from there

#### Scenario: Paused during the liveness check
- **WHEN** a human would pause a tlx max job whose search has ended and whose liveness check runs
- **THEN** the page offers no pause and the verb refuses it; a pause asked just before the search ended shows that no checkpoint is coming, and the job ends with its own outcome

### Requirement: A human can close the P slot
The page SHALL offer a toggle that closes and opens the P slot, kept on the state volume so it survives a runner restart and a reboot. While the P slot is closed, admission SHALL start and thaw nothing there, a job already running in it SHALL keep running unless it is thawed and not pausing (it is frozen again at once), and the E slot SHALL get the memory both slots would share.

#### Scenario: Other VMs need the performance cores
- **WHEN** a human closes the P slot while a job runs there
- **THEN** that job runs on to its end, nothing new starts in the P slot, and jobs run one at a time in the E slot

### Requirement: A human can hold the queue
The page's control that stops admission (the stopping mark) SHALL be named hold, with release to undo it, so that pause names only a job's pause.

#### Scenario: Holding the queue
- **WHEN** a human holds the queue while a job runs and others wait
- **THEN** the running job continues, nothing new starts, and release starts the next waiting job
