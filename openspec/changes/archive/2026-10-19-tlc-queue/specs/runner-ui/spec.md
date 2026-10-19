## ADDED Requirements

### Requirement: A web page shows the queue
The runner SHALL serve a web page, routed by pve's traefik at `tlcq.pve.local`, with no login. It SHALL show the two slots and what holds each (a job, a held restart, or nothing); the queue in its order, with each run's position; for each running job its progress (TLC's counters and their rates, the queue as a fraction of the distinct states and its growth over the last 5 minutes, the probe's numbers, its lane and the source of it, its time cap and time left, and its estimated end time with the source of the estimate); for each ended job its outcome (with the canceller's label and any "replaced by" id, or an error's reason), TLC's exit status, duration and logs; and for every job its id, configuration name and submitters (worktree and label). The page SHALL refresh itself from the runner's state every few seconds. The estimated end time SHALL use the estimators of the job-status spec on the curves of the runs the runner holds.

#### Scenario: Watching a long run
- **WHEN** a human opens the page while a max job runs in the 70% slot and two jobs wait
- **THEN** it shows the max job's counters, growth, time left and estimated end, and the two waiting jobs in their order

#### Scenario: An ended job's logs
- **WHEN** a human opens an ended job on the page
- **THEN** it shows the outcome, exit status and duration, and TLC's log

### Requirement: A human can cancel a job
The page SHALL offer cancel on every queued or running job. It SHALL do exactly what `tlcq cancel` does, with `ui` as the canceller's label, so every agent following the job reads `cancelled`, by `ui`, at its next poll.

#### Scenario: Cancelling from the page
- **WHEN** a human cancels a running job that an agent follows
- **THEN** the job's container stops, the job ends `cancelled`, and the agent's next `status` reports it cancelled by `ui` and exits 3

### Requirement: A human can reorder the queue
The page SHALL let a human move a queued run up, down, or to the head of the queue. A reorder SHALL change only the queue's order, under the admission lock; it SHALL NOT move a run that has a recorded attempt or a held restart, which keeps its place at the head, and new runs SHALL still join at the tail.

#### Scenario: Moving a job to the head
- **WHEN** slow jobs A, B and C wait in that order behind a running job, and a human moves C to the head
- **THEN** C starts when the running job ends, then A, then B

#### Scenario: A held restart stays first
- **WHEN** a run waits for its restart in the held 70% slot and a human moves another run to the head
- **THEN** the restart still starts first, and the moved run is next
