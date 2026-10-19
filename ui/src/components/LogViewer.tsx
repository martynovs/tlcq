import { useQuery } from "@tanstack/react-query";
import { ArrowDownToLine, X } from "lucide-react";
import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, type LogRef, type Run } from "../api";
import { short } from "../format";
import { Badge, Button } from "./ui";

/** An engine as its log names itself. */
export const ENGINES: Record<Run["engine"], string> = { tlx: "tlx", tlc: "TLC" };

/** How a log line (TLC's, or tlx's in its words) reads at a glance. */
function lineClass(line: string): string | undefined {
  if (/^Error|violated|Exception/.test(line)) return "text-red-600 dark:text-red-400";
  if (/^Progress\(|^Finished|states generated/.test(line)) return "text-indigo-700 dark:text-indigo-300";
  if (/^(Model checking completed|No error has been found)/.test(line)) return "text-emerald-700 dark:text-emerald-400";
  if (/^WARNING/.test(line)) return "text-zinc-400 dark:text-zinc-500";
  return undefined;
}

/** Colour lines only while the log is small enough to render line by line. */
const MAX_LINES = 5000;

/**
 * A run's engine log (its latest attempt's), shown under its job's row. While it
 * runs it is fetched again every few seconds and followed while the reader is
 * at its end; mounted anew (under Ended, say) it is always fetched once more,
 * so its last lines are there.
 */
export function LogViewer({ log, engine, attempt, live, onClose }: { log: LogRef; engine: Run["engine"]; attempt: number; live: boolean; onClose: () => void }) {
  const q = useQuery({
    queryKey: ["log", log.id, log.run, attempt],
    queryFn: () => api.log(log, attempt),
    refetchInterval: live ? 4000 : false,
    refetchOnMount: "always",
  });
  const pre = useRef<HTMLPreElement>(null);
  const [follow, setFollow] = useState(true);
  useLayoutEffect(() => {
    const el = pre.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [q.data, follow]);
  const lines = useMemo(() => q.data?.split("\n") ?? [], [q.data]);
  return (
    // No width of its own, so long lines scroll here instead of widening the table.
    <div className="w-0 min-w-full">
      <div className="overflow-hidden rounded-lg border border-zinc-200 dark:border-zinc-800">
        <div className="flex items-center gap-3 border-b border-zinc-200 bg-zinc-50 px-3 py-2 dark:border-zinc-800 dark:bg-zinc-900">
          <span className="text-xs font-semibold">{ENGINES[engine]} log</span>
          <span className="font-mono text-xs text-zinc-500">
            {short(log.id)} {log.run} · attempt {attempt}
          </span>
          {live && (
            <Badge tone="good" dot>
              live
            </Badge>
          )}
          {q.error && <span className="truncate text-xs text-red-600 dark:text-red-400">could not load: {q.error.message}</span>}
          <span className="ml-auto text-xs text-zinc-500 tabular-nums">{q.data != null && `${lines.length.toLocaleString()} lines`}</span>
          {!follow && (
            <Button onClick={() => setFollow(true)}>
              <ArrowDownToLine className="size-3.5" /> End
            </Button>
          )}
          <Button variant="ghost" size="icon" onClick={onClose} title="Close the log">
            <X className="size-4" />
          </Button>
        </div>
        <pre
          ref={pre}
          onScroll={(e) => {
            const el = e.currentTarget;
            setFollow(el.scrollTop + el.clientHeight >= el.scrollHeight - 8);
          }}
          className="h-[45vh] overflow-auto bg-white px-4 py-3 font-mono text-xs leading-relaxed text-zinc-800 dark:bg-zinc-950 dark:text-zinc-300"
        >
          {q.isPending
            ? "loading…"
            : lines.length > MAX_LINES
              ? q.data
              : lines.map((line, i) => (
                  <div key={i} className={lineClass(line)}>
                    {line || " "}
                  </div>
                ))}
        </pre>
      </div>
    </div>
  );
}
