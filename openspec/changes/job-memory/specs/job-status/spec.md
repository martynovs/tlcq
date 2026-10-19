## ADDED Requirements

### Requirement: A hint after running out of memory
The status line of a VM run that ended for lack of memory SHALL say how to rerun it.

- **tlx says how much.** For tlx's "liveness needs more memory: … the process needs about N MiB", the line SHALL add `needs more memory: resubmit with --memory <G>`. `<G>` is N scaled up by tlx's share of an attempt's memory (`tlx_mem_percent`), rounded up to a whole GB. The VM SHALL compute this number and send it with the result; the client SHALL print it.
- **Killed for memory.** For a run killed for memory (out of memory, or killed by signal 9 with nothing to say), the line SHALL add `needs more memory than its <M> MB`, where M is the memory its last attempt started with.

A waiting whole-VM run SHALL say it waits for the whole VM.

#### Scenario: tlx's estimate
- **WHEN** a run ends with "liveness needs more memory: … the process needs about 48049 MiB", and tlx gets 80% of an attempt's memory
- **THEN** its status line ends with `needs more memory: resubmit with --memory 59G`

#### Scenario: Killed for memory
- **WHEN** a run started with 54380 MB is killed by signal 9 with nothing to say
- **THEN** its status line ends with `needs more memory than its 54380 MB`

#### Scenario: Waiting for the whole VM
- **WHEN** a run asking for 96G waits at the head of the queue while a job runs in the P slot
- **THEN** `tlcq status` says it waits for the whole VM
