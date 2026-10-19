// The runner's page API (crates/tlcq-cli/src/runner/ui.rs): the state, ended
// runs a page at a time, an attempt's TLC log, and a human's cancel, reorder,
// freeze, unfreeze, pause and resume of a run, P-slot toggle, and hold and
// release of the queue.

export type Outcome =
  | { outcome: "ok" }
  | { outcome: "unexpected" }
  | { outcome: "timeout" }
  | { outcome: "cancelled"; by: string; replaced_by: string | null }
  | { outcome: "error"; reason: string | null }
  | { outcome: "never_sent" };

export interface Probe {
  distinct: number;
  queue: number;
  queue_before: number;
}

export interface LaneDecision {
  to: "slow" | "max";
  after_secs: number;
  probe: Probe | null;
}

export type JobResult = Outcome & {
  exit_status: number | null;
  distinct: number | null;
  generated: number | null;
  depth: number | null;
  place: string;
  duration_secs: number;
  lanes: LaneDecision[];
  errors: string[];
  note: string | null;
};

export interface Counters {
  generated: number;
  distinct: number;
  queue: number;
}

export interface Run {
  id: string;
  run: string;
  spec: string;
  /** The engine it runs on. */
  engine: "tlx" | "tlc";
  configs: string[];
  submitters: string[];
  /** frozen: its container paused; thawed: frozen, running on spare capacity; unfrozen,
   *  resuming: waiting first for a slot. */
  state: "queued" | "running" | "frozen" | "thawed" | "unfrozen" | "pausing" | "paused" | "resuming" | "stopping" | "ended";
  /** What a human may do to it besides cancel. */
  actions: RunAction[];
  /** Since when it is frozen, or when a paused run stopped at its checkpoint. */
  since: number | null;
  /** The slot it holds now; none while frozen, paused or waiting. */
  slot: "E" | "P" | null;
  /** Pausing after its search ended: no checkpoint comes, it ends on its own. */
  no_checkpoint: boolean;
  /** The memory its live attempt started with, in MB. */
  memory_mb: number | null;
  /** The disk its live attempt's scratch takes (spill, checkpoint), in MB. */
  scratch_mb: number | null;
  /** Its memory request, in MB (`--memory`). */
  request_mb: number | null;
  /** Paused on its checkpoint because it died (a long run, or a resume), and why. */
  died: string | null;
  position: number | null;
  movable: boolean;
  place: string | null;
  attempt: number | null;
  elapsed_secs: number | null;
  counters: Counters | null;
  generated_per_min: number | null;
  distinct_per_min: number | null;
  queue_share: number | null;
  queue_growth: number | null;
  queue_per_min: number | null;
  queue_slopes: number[];
  /** tlx's level being expanded: what is left of it, and its change per minute. */
  level: { depth: number; left: number; per_min: number | null } | null;
  expected: "short" | "long" | "max" | null;
  expected_source: string | null;
  lanes: LaneDecision[];
  left_secs: number | null;
  eta_secs: number | null;
  eta_source: "previous" | "edited" | "first_run" | null;
  /** The last known time: the latest complete run of its configuration took this long. */
  last_secs: number | null;
  /** Its depth now, and how deep it is likely to go: as deep as its last complete run, or from its own levels shrinking. */
  depth: { now: number; total: number | null; from: "previous" | "shrinking" | null } | null;
  cancelled_by: string | null;
  result: JobResult | null;
  started_at: number | null;
  ended_at: number | null;
}

export type RunAction = "freeze" | "unfreeze" | "pause" | "keep" | "resume";

export interface Slot {
  name: "E" | "P";
  open: boolean;
  /** thawed: a frozen job on spare capacity; whole: P kept empty for a whole-VM run's memory. */
  holder: "job" | "thawed" | "whole" | "free";
  run: string | null;
}

export interface State {
  slots: Slot[];
  queue: Run[];
  running: Run[];
  ended_total: number;
  /** The queue is held: nothing new starts until a release. */
  held: boolean;
  p_open: boolean;
  /** The state volume: free space and the floor below which no job starts, in MB. */
  disk: { free_mb: number; floor_mb: number } | null;
  /** The VM's free memory, in MB. */
  free_memory_mb: number | null;
  /** The memory a new attempt could start with now (free less the reserve and running jobs' room to grow), in MB. */
  room_mb: number | null;
  /** The most an attempt gets: a slot's share with both open, and E's alone (the whole VM), in MB. */
  shares: { slot_mb: number; whole_mb: number } | null;
  /** The least memory an attempt starts with, in MB. */
  min_mb: number;
  /** What queued runs wait for. */
  waits_for: string | null;
}

export interface EndedPage {
  total: number;
  page: number;
  per: number;
  runs: Run[];
}

/** The run whose log is shown: its latest attempt's, whichever that is now. */
export interface LogRef {
  id: string;
  run: string;
}

export type Move = "head" | "up" | "down";

async function answer(res: Response): Promise<Response> {
  if (!res.ok) throw new Error(`${res.status}: ${(await res.text()) || res.statusText}`);
  return res;
}

async function get<T>(path: string): Promise<T> {
  return (await answer(await fetch(path, { cache: "no-store" }))).json() as Promise<T>;
}

// The runner takes only JSON posts: a cross-site page cannot send that type
// without a CORS preflight, which the runner never answers.
async function post<T>(path: string, body: unknown): Promise<T> {
  const init = { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) };
  return (await answer(await fetch(path, init))).json() as Promise<T>;
}

export const api = {
  state: () => get<State>("/api/state"),
  ended: (page: number, per: number) => get<EndedPage>(`/api/ended?page=${page}&per=${per}`),
  // Not `/api/log`: browsers' privacy filters block that as a tracking beacon.
  log: async ({ id, run }: LogRef, attempt: number) =>
    (await answer(await fetch(`/api/output?id=${id}&run=${run}&attempt=${attempt}`, { cache: "no-store" }))).text(),
  curve: async ({ id, run }: LogRef, attempt: number) =>
    (await answer(await fetch(`/api/curve?id=${id}&run=${run}&attempt=${attempt}`, { cache: "no-store" }))).json() as Promise<
      [number, number, number, number][]
    >,
  cancel: (id: string, run: string) => post<unknown>("/api/cancel", { id, run }),
  reorder: (id: string, run: string, to: Move) => post<boolean>("/api/reorder", { id, run, to }),
  run: (action: RunAction, id: string, run: string) => post<boolean>(`/api/${action}`, { id, run }),
  pSlot: (open: boolean) => post<boolean>("/api/p-slot", { open }),
  hold: () => post<boolean>("/api/hold", {}),
  release: () => post<boolean>("/api/release", {}),
};

export const sameLog = (a: LogRef | null, r: Run) => a != null && a.id === r.id && a.run === r.run;

export const key = (r: { id: string; run: string }) => `${r.id}/${r.run}`;
