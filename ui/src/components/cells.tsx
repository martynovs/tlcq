// What several tables show the same way: a job, a moment, its lane, its outcome.
import clsx from "clsx";
import { User } from "lucide-react";
import type { JobResult, Run, State } from "../api";
import { ago, clock, dur, gb, num, short, stamp } from "../format";
import { Badge, Muted, Sparkline, Tip, type Tone } from "./ui";

/**
 * A job: its spec module, configurations and engine, who submitted it, and
 * its id. The submitter is cut to fit unless `full` (a table with room for it).
 */
export function Job({ r, full = false }: { r: Run; full?: boolean }) {
  const submitters = (
    <div className="flex items-center gap-1 text-xs text-zinc-500 dark:text-zinc-400">
      <User className="size-3 shrink-0" />
      <span className={clsx(!full && "truncate")}>{r.submitters.join(", ")}</span>
    </div>
  );
  return (
    <div className="min-w-48">
      <div className="flex items-baseline gap-1.5 whitespace-nowrap">
        <span className="font-medium text-zinc-900 dark:text-zinc-100">{r.spec}.tla</span>
        <span className="size-[3px] shrink-0 self-center rounded-full bg-zinc-400 dark:bg-zinc-500" />
        <span className="font-medium text-zinc-900 dark:text-zinc-100">{r.configs.join(", ") || short(r.id)}</span>
        <Badge tone={r.engine === "tlx" ? "accent" : "neutral"}>{r.engine}</Badge>
      </div>
      {r.submitters.length > 0 &&
        (full ? (
          <div className="mt-0.5">{submitters}</div>
        ) : (
          <Tip tip={r.submitters.join(", ")} className="mt-0.5 w-fit max-w-60">
            {submitters}
          </Tip>
        ))}
      <div className="mt-0.5 font-mono text-xs text-zinc-500 dark:text-zinc-400">
        {short(r.id)} {r.run}
      </div>
    </div>
  );
}

/** A moment: its time of day, how long ago, and on hover its full timestamp. */
export function Time({ secs }: { secs: number | null }) {
  if (secs == null) return <span className="text-zinc-400">—</span>;
  return (
    <Tip tip={stamp(secs)} className="w-fit cursor-default whitespace-nowrap">
      <div className="text-sm tabular-nums">{clock(secs)}</div>
      <Muted>{ago(secs)}</Muted>
    </Tip>
  );
}

const SOURCES: Record<string, string> = {
  flag: "by flag",
  suffix: "by its name",
  exact: "from its history",
  configuration: "from an earlier spec",
  name: "from its name's history",
};

/** A run's lane (or expected length), and why. */
function lane(r: Run): { name: string; why: string | null } {
  const d = r.lanes[r.lanes.length - 1];
  const name = d ? d.to : (r.expected ?? "unknown");
  const why = d
    ? d.probe
      ? `probe at ${dur(d.after_secs)}: queue ${num(d.probe.queue, true)} of ${num(d.probe.distinct, true)}`
      : `at ${dur(d.after_secs)}`
    : (r.expected_source && SOURCES[r.expected_source]) || null;
  return { name, why };
}

export function Lane({ r }: { r: Run }) {
  const { name, why } = lane(r);
  return (
    <div>
      <Badge tone={name === "unknown" ? "neutral" : name === "max" ? "accent" : "info"}>{name}</Badge>
      {why && <Muted className="mt-1">{why}</Muted>}
    </div>
  );
}

/** What admission weighs a run's start by: the VM's memory now. */
export type Memory = Pick<State, "room_mb" | "shares" | "min_mb" | "p_open">;

/**
 * The memory a run waiting to start needs and gets: at least its request
 * (else the least any attempt starts with), up to its slot's share (the
 * whole VM for a request above a slot's, or with the P slot closed); red
 * while the VM has less than it needs free for a new job.
 */
export function Wants({ r, m }: { r: Run; m: Memory }) {
  const need = r.request_mb ?? m.min_mb;
  const whole = !m.p_open || (m.shares != null && need > m.shares.slot_mb);
  const upTo = m.shares && (whole ? m.shares.whole_mb : m.shares.slot_mb);
  const short_ = m.room_mb != null && m.room_mb < need;
  return (
    <Muted className={clsx("mt-1 whitespace-nowrap", short_ && "text-red-600 dark:text-red-400")}>
      needs {r.request_mb != null ? gb(need) : `≥ ${gb(need)}`}
      {upTo != null && upTo > need && `, up to ${gb(upTo)}`}
      {r.request_mb != null && whole && m.p_open && " (whole VM)"}
    </Muted>
  );
}

/** Time so far against the cap, and when it started. */
export function Clock({ r }: { r: Run }) {
  if (r.elapsed_secs == null) return <span className="text-zinc-400">—</span>;
  const cap = r.left_secs == null ? null : r.elapsed_secs + r.left_secs;
  return (
    <div className="w-32">
      <div className="flex justify-between text-sm tabular-nums">
        <span className="font-medium">{dur(r.elapsed_secs)}</span>
        {cap != null && <span className="text-zinc-500">/ {dur(cap)}</span>}
      </div>
      {r.started_at != null && (
        <Tip tip={stamp(r.started_at)} className="mt-1 w-fit cursor-default">
          <Muted className="tabular-nums">since {clock(r.started_at)}</Muted>
        </Tip>
      )}
    </div>
  );
}

const OUTCOMES: Record<JobResult["outcome"], Tone> = {
  ok: "good",
  unexpected: "bad",
  error: "bad",
  timeout: "warn",
  cancelled: "neutral",
  never_sent: "neutral",
};

/** An ended run's outcome, and the line that says why. */
export function Outcome({ res }: { res: JobResult }) {
  const why =
    res.outcome === "cancelled"
      ? `by ${res.by}${res.replaced_by ? `, replaced by ${short(res.replaced_by)}` : ""}`
      : res.outcome === "error"
        ? res.reason
        : res.errors[0] && `${res.outcome === "ok" ? "as expected: " : ""}${res.errors[0].replace(/^Error:\s*/, "")}`;
  return (
    <div className="max-w-md">
      <div className="flex items-center gap-2">
        <Badge tone={OUTCOMES[res.outcome]} dot>
          {res.outcome.replace("_", " ")}
        </Badge>
        {res.exit_status != null && (
          <span className="font-mono text-xs text-zinc-500 tabular-nums dark:text-zinc-400" title="The engine's exit status (TLC's codes, which tlx keeps)">
            exit {res.exit_status}
          </span>
        )}
      </div>
      {why && <Muted className="mt-1">{why}</Muted>}
      {res.note && <Muted className="mt-0.5">{res.note}</Muted>}
    </div>
  );
}

/** A signed count per minute, compact: `+930K/min`, `−1.2M/min`. */
const signed = (v: number) => `${v < 0 ? "−" : "+"}${num(Math.abs(v), true)}`;
const perMin = (v: number) => `${signed(v)}/min`;

/**
 * The rates per minute: distinct states found, and the queue's slope (for
 * tlx, what is left of its level, draining instead); under them a chart of
 * the whole queue's slope over the last hour, a point a minute, even for tlx:
 * rising while the search widens faster, flat while steady, falling once it
 * slows or drains (below the dashed zero line it drains).
 */
export function Rate({ r }: { r: Run }) {
  if (r.distinct_per_min == null) return <span className="text-zinc-400">—</span>;
  return (
    <div className="w-32">
      <div className="flex justify-between text-sm tabular-nums">
        <span className="font-medium">{num(r.distinct_per_min, true)}</span>
        {r.level?.per_min != null ? (
          <span className="text-zinc-500">{signed(r.level.per_min)}</span>
        ) : (
          r.queue_per_min != null && <span className="text-zinc-500">{signed(r.queue_per_min)}</span>
        )}
      </div>
      {r.queue_per_min != null && (
        <div className="mt-1">
          <Sparkline values={r.queue_slopes} format={perMin} width={128} />
        </div>
      )}
    </div>
  );
}
