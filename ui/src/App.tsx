import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { api, key, type LogRef, type Move, type Run, type RunAction } from "./api";
import { Confirm } from "./components/Confirm";
import { Ended, HELD, Queue, Running, type Actions } from "./components/tables";
import { gb, short } from "./format";

const POLL = 4000;
const PER_PAGE = 10;

export function App() {
  const client = useQueryClient();
  const state = useQuery({ queryKey: ["state"], queryFn: api.state, refetchInterval: POLL });
  const [page, setPage] = useState(1);
  const ended = useQuery({
    queryKey: ["ended", page],
    queryFn: () => api.ended(page, PER_PAGE),
    refetchInterval: POLL,
    placeholderData: keepPreviousData,
  });
  const [log, setLog] = useState<LogRef | null>(null);
  const [chart, setChart] = useState<LogRef | null>(null);
  const [cancelling, setCancelling] = useState<Run | null>(null);
  // The latest action's failure, until dismissed or a later action succeeds.
  const [failed, setFailed] = useState<string | null>(null);
  const settle = { onError: (e: Error) => setFailed(e.message), onSettled: () => client.invalidateQueries() };
  const cancel = useMutation({ mutationFn: (r: Run) => api.cancel(r.id, r.run), onSuccess: () => setFailed(null), ...settle });
  const move = useMutation({
    mutationFn: ({ r, to }: { r: Run; to: Move }) => api.reorder(r.id, r.run, to),
    onSuccess: (moved: boolean) => setFailed(moved ? null : "That run can no longer move: it has started or left the queue."),
    ...settle,
  });
  const act = useMutation({
    mutationFn: ({ r, action }: { r: Run; action: RunAction }) => api.run(action, r.id, r.run),
    onSuccess: (done: boolean) => setFailed(done ? null : "That run has moved on: it can no longer do that."),
    ...settle,
  });
  const pSlot = useMutation({ mutationFn: api.pSlot, onSuccess: () => setFailed(null), ...settle });
  const hold = useMutation({ mutationFn: api.hold, onSuccess: () => setFailed(null), ...settle });
  const release = useMutation({ mutationFn: api.release, onSuccess: () => setFailed(null), ...settle });
  // A run that leaves Running has ended: fetch Ended now, so its row (and an
  // open log) appears there without waiting for the next poll.
  const running = useMemo(() => new Set(state.data?.running.map(key)), [state.data]);
  const before = useRef(running);
  useEffect(() => {
    if ([...before.current].some((k) => !running.has(k))) client.invalidateQueries({ queryKey: ["ended"] });
    before.current = running;
  }, [running, client]);
  const actions: Actions = {
    log,
    onLog: setLog,
    chart,
    onChart: setChart,
    onCancel: setCancelling,
    onMove: (r, to) => move.mutate({ r, to }),
    onRun: (r, action) => act.mutate({ r, action }),
    onPSlot: (open) => pSlot.mutate(open),
    onHold: () => hold.mutate(),
    onRelease: () => release.mutate(),
  };

  return (
    <div className="min-h-screen">
      <header className="sticky top-0 z-40 border-b border-zinc-200 bg-white/80 backdrop-blur dark:border-zinc-800 dark:bg-zinc-950/80">
        <div className="mx-auto flex max-w-7xl items-center gap-3 px-6 py-3">
          <div className="flex size-7 items-center justify-center rounded-lg bg-indigo-600 text-sm font-bold text-white">t</div>
          <div>
            <div className="text-sm leading-tight font-semibold">tlcq</div>
            <div className="text-xs leading-tight text-zinc-500 dark:text-zinc-400">TLA+ runner · {window.location.host}</div>
          </div>
          <div className="ml-auto flex items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400">
            {(state.data?.disk || state.data?.free_memory_mb != null) && (
              <span>
                {state.data.disk && <Disk disk={state.data.disk} />}
                {state.data.disk && state.data.free_memory_mb != null && " · "}
                {state.data.free_memory_mb != null && (
                  <span className="tabular-nums" title="The VM's available memory now (MemAvailable)">
                    memory {gb(state.data.free_memory_mb)} free
                  </span>
                )}
              </span>
            )}
            {state.isError ? (
              <>
                <span className="size-2 rounded-full bg-red-500 ring-4 ring-red-500/15" />
                <span className="text-red-600 dark:text-red-400">unreachable · {state.error.message}</span>
              </>
            ) : (
              <>
                <span className="relative flex size-2">
                  <span className="absolute inline-flex size-full animate-ping rounded-full bg-emerald-400 opacity-60" />
                  <span className="relative inline-flex size-2 rounded-full bg-emerald-500" />
                </span>
                <span className="tabular-nums">live · {state.dataUpdatedAt ? new Date(state.dataUpdatedAt).toLocaleTimeString() : "connecting…"}</span>
              </>
            )}
          </div>
        </div>
      </header>

      <main className="mx-auto max-w-7xl space-y-5 px-6 py-6">
        {failed && (
          <div className="flex items-center gap-2 rounded-lg border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700 dark:border-red-500/30 dark:bg-red-500/10 dark:text-red-300">
            <AlertTriangle className="size-4 shrink-0" />
            <span className="flex-1">{failed}</span>
            <button type="button" className="rounded p-0.5 hover:bg-red-100 dark:hover:bg-red-500/20" title="Dismiss" onClick={() => setFailed(null)}>
              <X className="size-4" />
            </button>
          </div>
        )}
        {state.data && (
          <>
            <Running
              runs={state.data.running.filter((r) => !HELD.has(r.state))}
              pOpen={state.data.p_open}
              pHeld={state.data.slots.find((s) => s.name === "P" && s.holder === "whole")?.run ?? null}
              a={actions}
            />
            {state.data.running.some((r) => HELD.has(r.state)) && (
              <Running title="Frozen and paused" empty="" runs={state.data.running.filter((r) => HELD.has(r.state))} m={state.data} a={actions} />
            )}
            <Queue runs={state.data.queue} held={state.data.held} waitsFor={state.data.waits_for} m={state.data} a={actions} />
          </>
        )}
        {state.isPending && <div className="py-24 text-center text-sm text-zinc-500">Loading the runner's state…</div>}
        <Ended data={ended.data} page={page} onPage={setPage} running={running} a={actions} />
      </main>

      {cancelling && (
        <Confirm
          title="Cancel this run?"
          body={`${cancelling.configs.join(", ")} (${short(cancelling.id)} ${cancelling.run}) will stop and end cancelled, by ui.`}
          action="Cancel run"
          onConfirm={() => cancel.mutate(cancelling)}
          onClose={() => setCancelling(null)}
        />
      )}
    </div>
  );
}

/** The VM's free disk: amber below twice the floor (the runner cleans up), red below it (nothing starts). */
function Disk({ disk }: { disk: { free_mb: number; floor_mb: number } }) {
  const tone =
    disk.free_mb < disk.floor_mb
      ? "text-red-600 dark:text-red-400"
      : disk.free_mb < 2 * disk.floor_mb
        ? "text-amber-600 dark:text-amber-400"
        : "";
  return (
    <span
      className={`tabular-nums ${tone}`}
      title={`No job starts below ${gb(disk.floor_mb)} free; below ${gb(2 * disk.floor_mb)} the runner deletes orphaned scratch and unused images`}
    >
      disk {gb(disk.free_mb)} free
    </span>
  );
}
