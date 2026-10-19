## ADDED Requirements

### Requirement: A run's memory request
A run with a memory request SHALL start only with at least that much memory, as admission computes a slot's memory (the slot's share, bounded by what the VM has free).

**A request a slot can serve.** A request at most one slot's share (half the VM's memory less the reserve with both slots open, all of it with only the E slot open) SHALL be served by any free open slot whose memory reaches it, the E slot first. A run whose request no free slot's memory reaches SHALL wait as a run short of memory does: at the head of the queue, the runs behind it waiting too, its free slot running a frozen job on spare capacity meanwhile.

**A whole-VM run.** A request above one slot's share with both slots open SHALL make a whole-VM run. It SHALL start only:
- in the E slot;
- while no job holds the P slot;
- with the memory the E slot gets with the P slot closed, if that reaches the request.

While it waits at the head of the queue for the P slot to free, the P slot SHALL start nothing.

**The hold on P.** While a whole-VM run has a live attempt that is not paused (running, thawed, frozen, unfrozen or pausing), admission SHALL treat the P slot as closed: it SHALL start and thaw nothing there, and SHALL freeze again at once a thawed job in it. The hold SHALL end when the run ends or is paused. A human's P toggle SHALL NOT end it.

**Refusal and records.** The VM SHALL refuse at commit a job whose request exceeds its memory less the reserve, naming that limit. Each attempt SHALL record the memory it started with, and whether it holds the whole VM.

#### Scenario: A request one slot serves
- **WHEN** the VM's slots each have 58 GB, the E slot is busy, and a run asking for 32G waits
- **THEN** it starts in the P slot with the P slot's memory

#### Scenario: Short of a request
- **WHEN** the P slot is free with 30 GB free for it and the run at the head of the queue asks for 48G
- **THEN** the run waits, the runs behind it wait too, and the P slot runs a frozen job on spare capacity if there is one

#### Scenario: A whole-VM run on an idle VM
- **WHEN** the VM is idle and a run asking for 96G is committed, its slots' share being 58 GB
- **THEN** it starts in the E slot with the memory of the whole VM less the reserve, and the next run committed waits instead of starting in the P slot

#### Scenario: A whole-VM run waits for P
- **WHEN** a job runs in the P slot, the E slot is free, and the run at the head of the queue asks for 96G
- **THEN** it waits, the P slot starts nothing when that job ends, and the run then starts in the E slot

#### Scenario: Frozen, the hold stays
- **WHEN** a human freezes a whole-VM run
- **THEN** the E slot frees for other work, and the P slot still starts and thaws nothing

#### Scenario: Paused, the hold ends
- **WHEN** a whole-VM run's pause lands
- **THEN** the P slot takes the next waiting run as usual

#### Scenario: More than the VM has
- **WHEN** a job asks for 200G on a VM with 125 GB of memory
- **THEN** the VM refuses the commit and says the most it can give
