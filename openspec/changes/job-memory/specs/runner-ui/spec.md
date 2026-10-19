## ADDED Requirements

### Requirement: The page shows a run's memory and a hold on P
The page SHALL show the memory a running attempt started with and any memory request of a run.

While a whole-VM run holds the P slot, the page SHALL show the P slot as held by that run, with the reason. The P toggle SHALL stay available, and SHALL say that the hold lasts until the run ends or pauses.

#### Scenario: A whole-VM run is live
- **WHEN** a whole-VM run runs in the E slot
- **THEN** the page shows the P slot held by it, "for its memory", and starts nothing there

#### Scenario: A request on a queued run
- **WHEN** a run asking for 64G waits in the queue
- **THEN** its row shows the request
