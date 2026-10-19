import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";
import { useMemo, useState } from "react";
import { api, type LogRef } from "../api";
import { dur, num, short } from "../format";
import { Button } from "./ui";

/**
 * A point of the curve: minutes into the attempt, then generated, distinct and
 * queued states; as totals from the runner, as rates per minute once charted.
 */
type Point = [number, number, number, number];

const SERIES = [
  { at: 1, title: "Generated", label: "generated", signed: false },
  { at: 2, title: "Distinct", label: "distinct", signed: false },
  { at: 3, title: "Queue", label: "queue growth", signed: true },
] as const;

const W = 320;
const H = 120;
const PAD = 4;

/** Each line's rates per minute since the line before it, at its own minute. */
function perMinute(points: Point[]): Point[] {
  return points.slice(1).map((p, i) => {
    const q = points[i] ?? p;
    const minutes = p[0] - q[0] || 1;
    return [p[0], (p[1] - q[1]) / minutes, (p[2] - q[2]) / minutes, (p[3] - q[3]) / minutes];
  });
}

/**
 * One rate over time: its own axis (they differ by orders of magnitude), a 2px
 * line, a dashed zero line when it goes below zero (a queue that drains).
 */
function Chart({
  points,
  at,
  title,
  label,
  signed,
  hover,
  onHover,
}: {
  points: Point[];
  at: 1 | 2 | 3;
  title: string;
  label: string;
  signed: boolean;
  hover: number | null;
  onHover: (i: number | null) => void;
}) {
  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[at]);
  const x0 = xs[0] ?? 0;
  const x1 = xs[xs.length - 1] ?? 1;
  const lo = Math.min(...ys, 0);
  const hi = Math.max(...ys, 0);
  const x = (v: number) => PAD + ((v - x0) / (x1 - x0 || 1)) * (W - 2 * PAD);
  const y = (v: number) => H - PAD - ((v - lo) / (hi - lo || 1)) * (H - 2 * PAD);
  const line = points.map((p) => `${x(p[0]).toFixed(1)},${y(p[at]).toFixed(1)}`).join(" ");
  const shown = hover ?? points.length - 1;
  const p = points[shown];
  const value = (v: number, short = false) => (signed ? `${v < 0 ? "−" : "+"}${num(Math.abs(v), short)}` : num(v, short));
  return (
    <figure className="min-w-0">
      <figcaption className="mb-1 flex items-baseline justify-between text-xs">
        <span className="font-semibold text-zinc-700 dark:text-zinc-200">
          {title} <span className="font-normal text-zinc-500">/min</span>
        </span>
        <span className="text-zinc-700 tabular-nums dark:text-zinc-200">{p ? value(p[at]) : ""}</span>
      </figcaption>
      <div className="relative">
        <svg
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          className="block h-28 w-full text-indigo-500 dark:text-indigo-400"
          role="img"
          aria-label={`${label} per minute over the run, now ${value(ys[ys.length - 1] ?? 0)}`}
          onMouseMove={(e) => {
            const box = e.currentTarget.getBoundingClientRect();
            const minute = x0 + ((e.clientX - box.left) / box.width) * (x1 - x0);
            let best = 0;
            xs.forEach((v, i) => {
              if (Math.abs(v - minute) < Math.abs((xs[best] ?? 0) - minute)) best = i;
            });
            onHover(best);
          }}
          onMouseLeave={() => onHover(null)}
        >
          <line x1={0} x2={W} y1={H - PAD} y2={H - PAD} className="stroke-zinc-200 dark:stroke-zinc-700" vectorEffect="non-scaling-stroke" />
          <line x1={0} x2={W} y1={PAD} y2={PAD} className="stroke-zinc-100 dark:stroke-zinc-800" vectorEffect="non-scaling-stroke" strokeDasharray="2 3" />
          {lo < 0 && (
            <line x1={0} x2={W} y1={y(0)} y2={y(0)} className="stroke-zinc-300 dark:stroke-zinc-600" vectorEffect="non-scaling-stroke" strokeDasharray="4 3" />
          )}
          <polyline points={line} fill="none" stroke="currentColor" strokeWidth={2} vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
          {p && hover != null && (
            <line x1={x(p[0])} x2={x(p[0])} y1={0} y2={H} className="stroke-zinc-400 dark:stroke-zinc-500" vectorEffect="non-scaling-stroke" />
          )}
        </svg>
        {p && hover != null && (
          <>
            {/* The point and its tooltip, in HTML: the stretched SVG would distort them. */}
            <span
              className="pointer-events-none absolute size-2 -translate-x-1/2 -translate-y-1/2 rounded-full bg-indigo-500 ring-2 ring-white dark:bg-indigo-400 dark:ring-zinc-900"
              style={{ left: `${(x(p[0]) / W) * 100}%`, top: `${(y(p[at]) / H) * 100}%` }}
            />
            <div
              className={`pointer-events-none absolute top-1 z-10 rounded-md border border-zinc-200 bg-white px-2 py-1 text-xs whitespace-nowrap shadow-md tabular-nums dark:border-zinc-700 dark:bg-zinc-900 ${
                x(p[0]) > W * 0.6 ? "-translate-x-[calc(100%+8px)]" : "translate-x-2"
              }`}
              style={{ left: `${(x(p[0]) / W) * 100}%` }}
            >
              <div className="font-medium text-zinc-900 dark:text-zinc-100">
                {value(p[at])} <span className="font-normal text-zinc-500">{label} /min</span>
              </div>
              <div className="text-zinc-500">at {dur(Math.round(p[0] * 60))}</div>
            </div>
          </>
        )}
      </div>
      <div className="mt-0.5 flex justify-between text-[11px] text-zinc-500 tabular-nums">
        <span>{lo < 0 ? `min ${value(lo, true)}` : "0"}</span>
        <span>max {value(hi, true)}</span>
      </div>
    </figure>
  );
}

/**
 * A run's progress under its row: states generated and distinct per minute,
 * and the queue's growth per minute, over the attempt, one chart each, a
 * shared crosshair. Refetched while it runs.
 */
export function ProgressCharts({ log, attempt, live, onClose }: { log: LogRef; attempt: number; live: boolean; onClose: () => void }) {
  const q = useQuery({
    queryKey: ["curve", log.id, log.run, attempt],
    queryFn: () => api.curve(log, attempt),
    refetchInterval: live ? 30_000 : false,
    refetchOnMount: "always",
  });
  const [hover, setHover] = useState<number | null>(null);
  const points = useMemo(() => perMinute((q.data ?? []) as Point[]), [q.data]);
  const at = points[hover ?? points.length - 1];
  return (
    <div className="w-0 min-w-full">
      <div className="rounded-lg border border-zinc-200 dark:border-zinc-800">
        <div className="flex items-center gap-3 border-b border-zinc-200 bg-zinc-50 px-3 py-2 dark:border-zinc-800 dark:bg-zinc-900">
          <span className="text-xs font-semibold">Progress</span>
          <span className="font-mono text-xs text-zinc-500">
            {short(log.id)} {log.run} · attempt {attempt}
          </span>
          {at && (
            <span className="text-xs text-zinc-500 tabular-nums">
              {hover == null ? "now" : "at"} {dur(Math.round(at[0] * 60))}
            </span>
          )}
          {q.error && <span className="truncate text-xs text-red-600 dark:text-red-400">could not load: {q.error.message}</span>}
          <Button variant="ghost" size="icon" className="ml-auto" onClick={onClose} title="Close the charts">
            <X className="size-4" />
          </Button>
        </div>
        <div className="grid gap-4 p-4 md:grid-cols-3">
          {q.isPending ? (
            <div className="text-sm text-zinc-500">loading…</div>
          ) : points.length < 2 ? (
            <div className="text-sm text-zinc-500">No progress lines yet.</div>
          ) : (
            SERIES.map((s) => <Chart key={s.at} points={points} {...s} hover={hover} onHover={setHover} />)
          )}
        </div>
      </div>
    </div>
  );
}
