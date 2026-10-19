/** A duration in seconds, as `45s`, `3m05s` or `2h10m`. */
export function dur(s: number | null | undefined): string {
  if (s == null) return "";
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = Math.floor(s % 60);
  if (h) return `${h}h${String(m).padStart(2, "0")}m`;
  if (m) return `${m}m${String(r).padStart(2, "0")}s`;
  return `${r}s`;
}

const integer = new Intl.NumberFormat();
const compact = new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 });

/** A count, with separators; compact (`1.2M`) when `short`. */
export const num = (n: number | null | undefined, short = false) =>
  n == null ? "" : short ? compact.format(n) : integer.format(Math.round(n));

/** An amount of memory given in MB, in GB with one decimal past ten. */
export const gb = (mb: number) => `${mb >= 10240 ? Math.round(mb / 1024) : (mb / 1024).toFixed(1)} GB`;

/** The full timestamp of `secs` (since the epoch), with its time zone. */
export const stamp = (secs: number) =>
  new Date(secs * 1000).toLocaleString(undefined, {
    weekday: "short",
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "short",
  });

/** The time of day of `secs`, with the date too when it is not today. */
export function clock(secs: number): string {
  const d = new Date(secs * 1000);
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return d.toDateString() === new Date().toDateString()
    ? time
    : `${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })}, ${time}`;
}

/** How long ago `secs` (since the epoch) was. */
export function ago(secs: number, now = Date.now() / 1000): string {
  const d = Math.max(0, now - secs);
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  return `${Math.floor(d / 86400)}d ago`;
}

export const short = (id: string) => id.slice(0, 12);
