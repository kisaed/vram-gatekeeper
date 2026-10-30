/** Small formatting helpers shared by the pages. */

/** Formats a MiB amount as MiB or GiB with sensible precision. */
export function mib(value: number | null | undefined): string {
  if (value === null || value === undefined) return 'unknown';
  if (value >= 1024) return `${(value / 1024).toFixed(value >= 10 * 1024 ? 0 : 1)} GiB`;
  return `${Math.round(value)} MiB`;
}

/** Formats seconds as a short duration such as "5 s", "3 min", "2 h". */
export function duration(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)} s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
  return `${(seconds / 3600).toFixed(1)} h`;
}

/** "Idle for 5 min" style phrase for a model that has not been used. */
export function idleFor(seconds: number): string {
  return seconds < 5 ? 'Recently used' : `Idle for ${duration(seconds)}`;
}

/** Clock time (HH:MM:SS) for the event feed. */
export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour12: false });
}

/** Percent of `part` in `whole`, clamped to 0-100. */
export function percent(part: number, whole: number): number {
  if (whole <= 0) return 0;
  return Math.max(0, Math.min(100, (part / whole) * 100));
}

/** Deep clone of plain JSON data. */
export function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** Formats a byte count as MiB or GiB. */
export function bytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(n >= 10 * 1024 ** 3 ? 0 : 1)} GiB`;
  return `${Math.max(0, Math.round(n / 1024 ** 2))} MiB`;
}

/** Transfer speed such as "12 MiB/s". */
export function speed(bps: number): string {
  return bps >= 1024 ** 2 ? `${(bps / 1024 ** 2).toFixed(1)} MiB/s` : `${Math.round(bps / 1024)} KiB/s`;
}

/** Time left for a transfer, or an empty string when it cannot be estimated. */
export function eta(remainingBytes: number, bps: number): string {
  if (bps < 1024 || remainingBytes <= 0) return '';
  return `${duration(remainingBytes / bps)} left`;
}
