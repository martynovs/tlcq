## ADDED Requirements

### Requirement: A memory request
A submission SHALL accept a memory request for its VM attempts in three places:
- `--memory <size>` on `submit`;
- a top-level `memory` key;
- a per-configuration `memory` field in the TOML config.

These layer like the other per-configuration fields: the flag wins over every configuration, and in a file a configuration's field beats the top-level key.

A size SHALL be an integer with an optional `M`, `G` or `T` suffix, in powers of 1024 (a bare integer is MB). tlcq SHALL refuse, exiting 2:
- a size it cannot read;
- a size below the least an attempt starts with (4096 MB).

A request SHALL be a flag of the run, kept from its first submitter as `--timeout` is: an attach with another request SHALL be warned and SHALL NOT change the run's. A request SHALL NOT be part of the job's id.

tlcq SHALL refuse, exiting 2, a job with a request that would run on the laptop: under `--local`, or placed there by its expected length. A job with a request SHALL be committed to the VM's queue instead of the laptop's slot.

#### Scenario: A configuration asks for memory
- **WHEN** `tlcq.toml` gives `{ name = "RP_live_max.cfg", memory = "96G" }` and an agent runs `tlcq submit replicas RP_live_max.cfg`
- **THEN** the job is committed to the VM with a request of 98304 MB

#### Scenario: The flag beats the configuration
- **WHEN** `tlcq.toml` gives `{ name = "RP_live_max.cfg", memory = "96G" }` and an agent runs `tlcq submit replicas RP_live_max.cfg --memory 64G`
- **THEN** the job's request is 65536 MB

#### Scenario: The same id with a request
- **WHEN** a job ended `error` with "liveness needs more memory" and an agent submits it again with `--memory 64G`
- **THEN** its id is unchanged and a new run of it starts on the VM with the request

#### Scenario: An attach asks for more
- **WHEN** a run of a job is queued with no request and a second agent submits the job with `--memory 64G`
- **THEN** the second agent attaches, is warned that the run keeps its first submitter's flags, and the run has no request

#### Scenario: A request on the laptop
- **WHEN** an agent runs `tlcq submit replicas RP_live.cfg --local --memory 64G`
- **THEN** tlcq exits 2 and says the memory request is for VM runs

#### Scenario: A short job with a request
- **WHEN** a short job whose configuration asks for `16G` is submitted
- **THEN** it is committed to the VM's queue, not run in the laptop's slot

#### Scenario: Too little
- **WHEN** an agent runs `tlcq submit replicas RP_live.cfg --memory 2G`
- **THEN** tlcq exits 2 and says a request is at least 4096 MB
