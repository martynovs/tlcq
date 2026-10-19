import clsx from "clsx";
import { ArrowDown, ArrowUp, ArrowUpToLine, ChartLine, ChevronLeft, ChevronRight, Cpu, Flame, Pause, Play, ScrollText, Snowflake, Undo2, X } from "lucide-react";
import { Fragment, useState } from "react";
import { key, sameLog, type EndedPage, type LogRef, type Move, type Run, type RunAction } from "../api";
import { ago, dur, gb, num, short } from "../format";
import { Clock, Job, Lane, Outcome, Rate, Time, Wants, type Memory } from "./cells";
import { ENGINES, LogViewer } from "./LogViewer";
import { ProgressCharts } from "./ProgressCharts";
import { Badge, Button, Card, Empty, Muted, Table, Td } from "./ui";

export interface Actions {
  log: LogRef | null;
  onLog: (log: LogRef | null) => void;
  chart: LogRef | null;
  onChart: (chart: LogRef | null) => void;
  onCancel: (r: Run) => void;
  onMove: (r: Run, to: Move) => void;
  onRun: (r: Run, action: RunAction) => void;
  onPSlot: (open: boolean) => void;
  onHold: () => void;
  onRelease: () => void;
}

/** The log button of a row: opens its log under it, or closes it. */
function LogButton({ r, a }: { r: Run; a: Actions }) {
  if (r.attempt == null) return null;
  const open = sameLog(a.log, r);
  return (
    <Button
      variant={open ? "primary" : "default"}
      size="icon"
      onClick={() => a.onLog(open ? null : { id: r.id, run: r.run })}
      title={open ? "Log: close it" : `Log: show the ${ENGINES[r.engine]} log`}
    >
      <ScrollText className="size-3.5" />
    </Button>
  );
}

/** The log under a row, when it is the one open; followed while its engine runs. */
function LogRow({ r, a, columns }: { r: Run; a: Actions; columns: number }) {
  if (!a.log || !sameLog(a.log, r) || r.attempt == null) return null;
  const live = LIVE.has(r.state);
  return (
    <tr className="bg-zinc-50/60 dark:bg-zinc-900/40">
      <td colSpan={columns} className="px-4 pt-1 pb-4">
        <LogViewer log={a.log} engine={r.engine} attempt={r.attempt} live={live} onClose={() => a.onLog(null)} />
      </td>
    </tr>
  );
}

/** The chart button of a row: opens its progress charts under it, or closes them. */
function ChartButton({ r, a }: { r: Run; a: Actions }) {
  if (r.attempt == null) return null;
  const open = sameLog(a.chart, r);
  return (
    <Button
      variant={open ? "primary" : "default"}
      size="icon"
      onClick={() => a.onChart(open ? null : { id: r.id, run: r.run })}
      title={open ? "Chart: close it" : "Chart: generated, distinct and queued states"}
    >
      <ChartLine className="size-3.5" />
    </Button>
  );
}

/** The progress charts under a row, when they are the ones open. */
function ChartRow({ r, a, columns }: { r: Run; a: Actions; columns: number }) {
  if (!a.chart || !sameLog(a.chart, r) || r.attempt == null) return null;
  const live = LIVE.has(r.state);
  return (
    <tr className="bg-zinc-50/60 dark:bg-zinc-900/40">
      <td colSpan={columns} className="px-4 pt-1 pb-4">
        <ProgressCharts log={a.chart} attempt={r.attempt} live={live} onClose={() => a.onChart(null)} />
      </td>
    </tr>
  );
}

/** A secondary figure: its value stands out, its label does not. */
const Figure = ({ value, label }: { value: string; label: string }) => (
  <div className="text-xs">
    <span className="font-medium text-zinc-700 dark:text-zinc-200">{value}</span> <span className="text-zinc-500">{label}</span>
  </div>
);

/** The states of the Frozen and paused card: runs that wait in no slot. */
export const HELD = new Set<Run["state"]>(["frozen", "unfrozen", "paused", "resuming"]);

/** The states whose engine runs: their log and charts follow it. */
const LIVE = new Set<Run["state"]>(["running", "thawed", "pausing", "stopping"]);

/** Queued runs a page. */
const QUEUE_PAGE = 10;

const STATES: Partial<Record<Run["state"], string>> = {
  frozen: "frozen",
  thawed: "on spare capacity",
  unfrozen: "unfreezing: waits for a slot",
  pausing: "pausing at next checkpoint",
  paused: "paused",
  resuming: "resuming: waits for a slot",
  stopping: "stopping",
};

/** A run's buttons for what a human may do to it besides cancel. */
const ACTIONS: Record<RunAction, { label: string; title: string; icon: typeof Pause; tint?: string }> = {
  freeze: { label: "Freeze", title: "Pause its container now: its slot frees, its memory stays", icon: Snowflake, tint: "text-sky-600 dark:text-sky-400" },
  unfreeze: { label: "Unfreeze", title: "Run it again: first for the next free slot, never frozen again for new work", icon: Flame, tint: "text-amber-600 dark:text-amber-400" },
  pause: { label: "Pause", title: "Stop it at its next checkpoint, kept until a resume (survives reboots)", icon: Pause },
  keep: { label: "Keep running", title: "Withdraw the pause: it runs on in its slot", icon: Undo2 },
  resume: { label: "Resume", title: "Recover its checkpoint first in the queue, in whichever slot frees", icon: Play },
};

/** Unfreeze on a thawed run: it already runs, so the verb only keeps its slot. */
const KEEP_IN_SLOT = { ...ACTIONS.unfreeze, label: "Keep in slot", title: "Run on as a normal job, not frozen again for new work" };

/**
 * Runs in a table: the running ones (with the P toggle when `pOpen` is given),
 * or the frozen and paused ones, which hold no slot.
 */
export function Running({
  runs,
  pOpen,
  pHeld = null,
  title = "Running",
  empty = "Nothing is running",
  m,
  a,
}: {
  runs: Run[];
  pOpen?: boolean;
  /** The whole-VM run that keeps P empty for its memory. */
  pHeld?: string | null;
  title?: string;
  empty?: string;
  /** The VM's memory now, for what a run waiting to start needs. */
  m?: Memory;
  a: Actions;
}) {
  const rate = (
    <>
      Rate <span className="normal-case">/min</span>
    </>
  );
  const head = ["Job", "Slot", "Time", "States", rate, "ETA", ""];
  // Closed, the P slot starts and thaws nothing, for pve's other VMs.
  const toggle = pOpen != null && (
    <Button
      variant={pOpen ? "default" : "primary"}
      onClick={() => a.onPSlot(!pOpen)}
      title={
        (pOpen ? "Close the P slot: its performance cores go to pve's other VMs; a job running there runs on" : "Open the P slot to jobs again") +
        (pHeld ? `. Either way it stays empty for ${short(pHeld)}'s memory until that run ends or pauses` : "")
      }
    >
      <Cpu className="size-3.5" /> P cores: {pOpen ? "open" : "closed"}
      {pHeld && <span className="font-normal text-zinc-500">· held for {short(pHeld)}'s memory</span>}
    </Button>
  );
  return (
    <Card title={title} count={runs.length} actions={toggle || undefined}>
      <Table head={head}>
        {runs.length === 0 && <Empty columns={head.length}>{empty}</Empty>}
        {runs.map((r) => (
          <Fragment key={`${r.id}/${r.run}`}>
            <tr className="hover:bg-zinc-50/70 dark:hover:bg-zinc-800/30">
              <Td>
                <Job r={r} />
              </Td>
              <Td>
                {r.slot ? <Badge tone="info">{r.slot}</Badge> : <Muted className="whitespace-nowrap">no slot</Muted>}
                {r.memory_mb != null ? (
                  <Muted className="mt-1 whitespace-nowrap">{`mem: ${gb(r.memory_mb)}`}</Muted>
                ) : (
                  m && (r.state === "paused" || r.state === "resuming") && <Wants r={r} m={m} />
                )}
                {r.scratch_mb != null && r.scratch_mb >= 1024 && (
                  <Muted className="whitespace-nowrap">disk {gb(r.scratch_mb)}</Muted>
                )}
                {STATES[r.state] && (
                  <Muted className="mt-1 max-w-56">
                    {r.state === "pausing" && r.no_checkpoint
                      ? "search done: ends on its own"
                      : r.state === "pausing" && !r.slot
                        ? "pausing now: last checkpoint"
                        : r.state === "paused" && r.since != null
                          ? `${r.died ? "died" : "paused"}: ${ago(r.since)}`
                          : STATES[r.state]}
                    {r.cancelled_by && ` · by ${r.cancelled_by}`}
                    {r.died && <div className="text-amber-700 dark:text-amber-400">{r.died}</div>}
                    {r.since != null && r.state !== "paused" && <div>{`since ${ago(r.since)}`}</div>}
                  </Muted>
                )}
              </Td>
              <Td>
                <Clock r={r} />
              </Td>
              <Td className="whitespace-nowrap tabular-nums">
                {r.counters ? (
                  <>
                    <div>
                      {num(r.counters.distinct, true)} <span className="text-xs text-zinc-500">distinct</span>
                    </div>
                    <Figure value={num(r.counters.generated, true)} label="generated" />
                    <Figure value={num(r.counters.queue, true)} label="queued" />
                  </>
                ) : (
                  <span className="text-zinc-400">—</span>
                )}
              </Td>
              <Td className="whitespace-nowrap tabular-nums">
                <Rate r={r} />
              </Td>
              <Td className="whitespace-nowrap tabular-nums">
                {r.eta_secs == null ? (
                  <span className="text-zinc-400">—</span>
                ) : (
                  <>
                    <div>
                      <span className="text-xs text-zinc-500">in</span> <span className="font-medium">{dur(r.eta_secs)}</span>
                    </div>
                    <Muted>{{ previous: "from its last run", edited: "from an earlier version", first_run: "first-run estimate" }[r.eta_source ?? "first_run"]}</Muted>
                  </>
                )}
                {r.last_secs != null && <Muted>last run {dur(r.last_secs)}</Muted>}
                {r.depth && (
                  <Muted>
                    level {r.depth.now}
                    {r.depth.total != null && r.depth.total > r.depth.now && ` of ~${r.depth.total}`}
                    {r.level && r.level.depth === r.depth.now && `: ${num(r.level.left, true)}`}
                  </Muted>
                )}
              </Td>
              <Td className="text-right whitespace-nowrap">
                <div className="flex justify-end gap-1">
                  {r.actions.map((action) => {
                    const { label, title, icon: Icon, tint } = r.state === "thawed" && action === "unfreeze" ? KEEP_IN_SLOT : ACTIONS[action];
                    return (
                      <Button key={action} size="icon" onClick={() => a.onRun(r, action)} title={`${label}: ${title}`}>
                        <Icon className={clsx("size-3.5", tint)} />
                      </Button>
                    );
                  })}
                  <ChartButton r={r} a={a} />
                  <LogButton r={r} a={a} />
                  {r.state !== "stopping" && (
                    <Button variant="danger" size="icon" onClick={() => a.onCancel(r)} title="Cancel this run">
                      <X className="size-3.5" />
                    </Button>
                  )}
                </div>
              </Td>
            </tr>
            <LogRow r={r} a={a} columns={head.length} />
            <ChartRow r={r} a={a} columns={head.length} />
          </Fragment>
        ))}
      </Table>
    </Card>
  );
}

export function Queue({ runs, held, waitsFor, m, a }: { runs: Run[]; held: boolean; waitsFor: string | null; m: Memory; a: Actions }) {
  const head = ["#", "Job", "Expected", ""];
  const [wanted, setPage] = useState(1);
  // A page past the end, as the queue drains, shows the last one.
  const page = Math.min(wanted, Math.max(1, Math.ceil(runs.length / QUEUE_PAGE)));
  const first = (page - 1) * QUEUE_PAGE;
  // Held, no queued job starts; running jobs go on either way.
  const hold = held ? (
    <>
      <span className="flex items-center gap-1.5 text-xs font-medium text-amber-700 dark:text-amber-400">
        <Pause className="size-3.5" /> Held: nothing new starts
      </span>
      <Button variant="primary" onClick={a.onRelease} title="Start admitting the queue again">
        <Play className="size-3.5" /> Release
      </Button>
    </>
  ) : (
    <>
      {runs.length > 0 && waitsFor && (
        <span className="text-xs text-zinc-500" title={waitsFor === "memory" ? "Frozen jobs keep their memory: pause one to free it" : undefined}>
          waiting for {waitsFor}
          {m.room_mb != null && ` · ${gb(m.room_mb)} free for a new job`}
        </span>
      )}
      <Button onClick={a.onHold} title="Start no queued job until released; running jobs go on">
        <Pause className="size-3.5" /> Hold queue
      </Button>
    </>
  );
  return (
    <Card
      title="Queue"
      count={runs.length}
      actions={
        <>
          {hold}
          {runs.length > QUEUE_PAGE && <Pager total={runs.length} per={QUEUE_PAGE} page={page} onPage={setPage} back="Earlier" on="Later" />}
        </>
      }
    >
      <Table head={head}>
        {runs.length === 0 && <Empty columns={head.length}>The queue is empty</Empty>}
        {runs.slice(first, first + QUEUE_PAGE).map((r, at) => (
          <tr key={`${r.id}/${r.run}`} className="hover:bg-zinc-50/70 dark:hover:bg-zinc-800/30">
            <Td className="w-10 text-zinc-500 tabular-nums">{r.position}</Td>
            <Td>
              <Job r={r} full />
            </Td>
            <Td>
              <Lane r={r} />
              <Wants r={r} m={m} />
            </Td>
            <Td className="text-right whitespace-nowrap">
              <div className="flex justify-end gap-1">
                {r.movable && (
                  <>
                    <Button size="icon" variant="ghost" title="Move to the head of the queue" disabled={first + at === 0} onClick={() => a.onMove(r, "head")}>
                      <ArrowUpToLine className="size-4" />
                    </Button>
                    <Button size="icon" variant="ghost" title="Move up" disabled={first + at === 0} onClick={() => a.onMove(r, "up")}>
                      <ArrowUp className="size-4" />
                    </Button>
                    <Button size="icon" variant="ghost" title="Move down" disabled={first + at === runs.length - 1} onClick={() => a.onMove(r, "down")}>
                      <ArrowDown className="size-4" />
                    </Button>
                  </>
                )}
                <Button variant="danger" className="ml-1.5" onClick={() => a.onCancel(r)} title="Cancel this run">
                  <X className="size-3.5" /> Cancel
                </Button>
              </div>
            </Td>
          </tr>
        ))}
      </Table>
    </Card>
  );
}

/** Page `page` of `total` rows, `per` a page; `back` and `on` name the buttons. */
function Pager({ total, per, page, onPage, back, on }: { total: number; per: number; page: number; onPage: (p: number) => void; back: string; on: string }) {
  const pages = Math.max(1, Math.ceil(total / per));
  const from = total === 0 ? 0 : (page - 1) * per + 1;
  const to = Math.min(total, page * per);
  return (
    <div className="flex items-center gap-2 text-xs text-zinc-500 tabular-nums">
      <span>
        {from}–{to} of {total}
      </span>
      <Button size="icon" title={back} disabled={page <= 1} onClick={() => onPage(page - 1)}>
        <ChevronLeft className="size-4" />
      </Button>
      <span>
        page {page} of {pages}
      </span>
      <Button size="icon" title={on} disabled={page >= pages} onClick={() => onPage(page + 1)}>
        <ChevronRight className="size-4" />
      </Button>
    </div>
  );
}

/** Ended runs a page at a time; a run still listed as running keeps its log there. */
export function Ended({
  data,
  page,
  onPage,
  running,
  a,
}: {
  data: EndedPage | undefined;
  page: number;
  onPage: (p: number) => void;
  running: Set<string>;
  a: Actions;
}) {
  const head = ["Job", "Outcome", "Duration", "States", "Ended", ""];
  const total = data?.total ?? 0;
  const per = data?.per ?? 10;
  const pager = <Pager total={total} per={per} page={page} onPage={onPage} back="Newer" on="Older" />;
  return (
    <Card title="Ended" count={total} actions={pager}>
      <Table head={head}>
        {data && data.runs.length === 0 && <Empty columns={head.length}>Nothing has ended yet</Empty>}
        {data?.runs.map((r) => (
          <Fragment key={`${r.id}/${r.run}`}>
            <tr className="hover:bg-zinc-50/70 dark:hover:bg-zinc-800/30">
              <Td>
                <Job r={r} />
              </Td>
              <Td>{r.result && <Outcome res={r.result} />}</Td>
              <Td className="tabular-nums">
                <div>{dur(r.result?.duration_secs)}</div>
                {r.result?.place && <Muted>{r.result.place}</Muted>}
              </Td>
              <Td className="tabular-nums">
                {r.result?.distinct == null ? (
                  <span className="text-zinc-400">—</span>
                ) : (
                  <>
                    <div>
                      {num(r.result.distinct)} <span className="text-xs text-zinc-500">distinct</span>
                    </div>
                    {r.result.depth != null && <Muted>depth {r.result.depth}</Muted>}
                  </>
                )}
              </Td>
              <Td>
                <Time secs={r.ended_at} />
              </Td>
              <Td className="text-right">
                <div className="flex justify-end gap-1.5">
                  <ChartButton r={r} a={a} />
                  <LogButton r={r} a={a} />
                </div>
              </Td>
            </tr>
            {!running.has(key(r)) && <LogRow r={r} a={a} columns={head.length} />}
            {!running.has(key(r)) && <ChartRow r={r} a={a} columns={head.length} />}
          </Fragment>
        ))}
      </Table>
      {total > per && <div className="flex justify-end border-t border-zinc-200 px-4 py-2.5 dark:border-zinc-800">{pager}</div>}
    </Card>
  );
}
