// Small building blocks in the shadcn/ui manner: plain elements with Tailwind.
import clsx from "clsx";
import { useState, type ButtonHTMLAttributes, type ReactNode } from "react";

export type Tone = "neutral" | "good" | "bad" | "warn" | "info" | "accent";

const tones: Record<Tone, string> = {
  neutral: "bg-zinc-100 text-zinc-700 ring-zinc-200 dark:bg-zinc-800/60 dark:text-zinc-300 dark:ring-zinc-700",
  good: "bg-emerald-50 text-emerald-700 ring-emerald-200 dark:bg-emerald-500/10 dark:text-emerald-300 dark:ring-emerald-500/25",
  bad: "bg-red-50 text-red-700 ring-red-200 dark:bg-red-500/10 dark:text-red-300 dark:ring-red-500/25",
  warn: "bg-amber-50 text-amber-800 ring-amber-200 dark:bg-amber-500/10 dark:text-amber-300 dark:ring-amber-500/25",
  info: "bg-sky-50 text-sky-700 ring-sky-200 dark:bg-sky-500/10 dark:text-sky-300 dark:ring-sky-500/25",
  accent: "bg-indigo-50 text-indigo-700 ring-indigo-200 dark:bg-indigo-500/10 dark:text-indigo-300 dark:ring-indigo-500/25",
};

export function Badge({ tone = "neutral", children, dot }: { tone?: Tone; children: ReactNode; dot?: boolean }) {
  return (
    <span className={clsx("inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset whitespace-nowrap", tones[tone])}>
      {dot && <span className="size-1.5 rounded-full bg-current" />}
      {children}
    </span>
  );
}

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "default" | "ghost" | "danger" | "primary";
  size?: "sm" | "icon";
};

export function Button({ variant = "default", size = "sm", className, ...rest }: ButtonProps) {
  return (
    <button
      type="button"
      className={clsx(
        "inline-flex items-center justify-center gap-1.5 rounded-md text-xs font-medium transition-colors",
        "focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-indigo-500 disabled:pointer-events-none disabled:opacity-40",
        size === "sm" ? "h-7 px-2.5" : "size-7",
        variant === "default" &&
          "border border-zinc-200 bg-white text-zinc-700 shadow-xs hover:bg-zinc-50 hover:text-zinc-900 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:bg-zinc-800 dark:hover:text-white",
        variant === "ghost" && "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-zinc-800 dark:hover:text-white",
        variant === "danger" &&
          "border border-zinc-200 bg-white text-zinc-600 hover:border-red-300 hover:bg-red-50 hover:text-red-700 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-300 dark:hover:border-red-500/40 dark:hover:bg-red-500/10 dark:hover:text-red-300",
        variant === "primary" && "bg-indigo-600 text-white shadow-xs hover:bg-indigo-500",
        className,
      )}
      {...rest}
    />
  );
}

export function Card({ title, count, actions, children }: { title: string; count?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="overflow-hidden rounded-xl border border-zinc-200 bg-white shadow-xs dark:border-zinc-800 dark:bg-zinc-900">
      <header className="flex items-center gap-2 border-b border-zinc-200 px-4 py-3 dark:border-zinc-800">
        <h2 className="text-sm font-semibold">{title}</h2>
        {count != null && (
          <span className="rounded-full bg-zinc-100 px-2 text-xs font-medium text-zinc-600 tabular-nums dark:bg-zinc-800 dark:text-zinc-400">{count}</span>
        )}
        <div className="ml-auto flex items-center gap-2">{actions}</div>
      </header>
      {children}
    </section>
  );
}

/** A table whose columns are given once; rows come as children. */
export function Table({ head, children }: { head: ReactNode[]; children: ReactNode }) {
  return (
    // Sideways scrolling only, for a narrow window: never a scrollbar of its own downwards.
    <div className="overflow-x-auto overflow-y-hidden">
      <table className="w-full text-left text-sm">
        <thead>
          <tr className="border-b border-zinc-200 bg-zinc-50/80 dark:border-zinc-800 dark:bg-zinc-900/60">
            {head.map((h, i) => (
              <th key={i} className="px-3.5 py-2 text-[11px] font-semibold tracking-wider whitespace-nowrap text-zinc-500 uppercase dark:text-zinc-400">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="divide-y divide-zinc-100 dark:divide-zinc-800">{children}</tbody>
      </table>
    </div>
  );
}

export const Td = ({ children, className }: { children?: ReactNode; className?: string }) => (
  <td className={clsx("px-3.5 py-3 align-top", className)}>{children}</td>
);

export function Empty({ columns, children }: { columns: number; children: ReactNode }) {
  return (
    <tr>
      <td colSpan={columns} className="px-4 py-10 text-center text-sm text-zinc-500 dark:text-zinc-400">
        {children}
      </td>
    </tr>
  );
}

/** `children`, with `tip` in a popup above them while hovered. */
export function Tip({ tip, children, className }: { tip: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className={clsx("group/tip relative", className)}>
      {children}
      <div
        role="tooltip"
        className="pointer-events-none absolute bottom-full left-0 z-30 mb-1.5 rounded-md bg-zinc-900 px-2.5 py-1.5 text-xs font-medium whitespace-nowrap text-white shadow-lg hidden group-hover/tip:block dark:bg-zinc-100 dark:text-zinc-900"
      >
        {tip}
      </div>
    </div>
  );
}

export const Muted =({ children, className }: { children: ReactNode; className?: string }) => (
  <div className={clsx("text-xs text-zinc-500 dark:text-zinc-400", className)}>{children}</div>
);

/**
 * A small line chart of `values`, oldest first, one a minute: a 2px line, a
 * dashed zero line when they cross it, the latest value marked. Hovering
 * shows a crosshair on the nearest point with its value (`format`) and age.
 */
export function Sparkline({
  values,
  format,
  width = 100,
  height = 28,
}: {
  values: number[];
  format: (v: number) => string;
  width?: number;
  height?: number;
}) {
  const [at, setAt] = useState<number | null>(null);
  if (values.length < 2) return null;
  const lo = Math.min(0, ...values) < 0 && Math.max(...values) > 0 ? Math.min(...values) : Math.min(...values);
  const hi = Math.max(...values);
  const span = hi - lo || 1;
  const pad = 4;
  const x = (i: number) => pad + (i / (values.length - 1)) * (width - 2 * pad);
  const y = (v: number) => height - pad - ((v - lo) / span) * (height - 2 * pad);
  const points = values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const last = values.length - 1;
  const shown = at ?? last;
  const value = values[shown] ?? 0;
  return (
    <div className="relative w-fit">
      <svg
        width={width}
        height={height}
        className="block text-indigo-500 dark:text-indigo-400"
        role="img"
        aria-label={`last ${values.length} minutes, now ${format(values[last] ?? 0)}`}
        onMouseMove={(e) => {
          const box = e.currentTarget.getBoundingClientRect();
          const i = Math.round(((e.clientX - box.left - pad) / (width - 2 * pad)) * last);
          setAt(Math.max(0, Math.min(last, i)));
        }}
        onMouseLeave={() => setAt(null)}
      >
        {lo < 0 && hi > 0 && (
          <line x1={0} x2={width} y1={y(0)} y2={y(0)} className="stroke-zinc-300 dark:stroke-zinc-600" strokeDasharray="2 2" />
        )}
        <polyline points={points} fill="none" stroke="currentColor" strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
        {at != null && <line x1={x(at)} x2={x(at)} y1={0} y2={height} className="stroke-zinc-400 dark:stroke-zinc-500" strokeWidth={1} />}
        <circle cx={x(shown)} cy={y(value)} r={4} fill="currentColor" className="stroke-white dark:stroke-zinc-900" strokeWidth={2} />
      </svg>
      {at != null && (
        <div className="pointer-events-none absolute bottom-full left-1/2 z-30 mb-1 -translate-x-1/2 rounded-md bg-zinc-900 px-2 py-1 text-xs font-medium whitespace-nowrap text-white shadow-lg dark:bg-zinc-100 dark:text-zinc-900">
          {format(value)} <span className="opacity-70">{at === last ? "now" : `≈${last - at}m ago`}</span>
        </div>
      )}
    </div>
  );
}
