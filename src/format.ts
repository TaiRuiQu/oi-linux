const KIB = 1024;
const MIB = KIB * 1024;
const GIB = MIB * 1024;

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < KIB) return `${bytes} B`;
  if (bytes < MIB) return `${(bytes / KIB).toFixed(1)} KiB`;
  if (bytes < GIB) return `${(bytes / MIB).toFixed(1)} MiB`;
  return `${(bytes / GIB).toFixed(2)} GiB`;
}

export function formatUnixSeconds(unixSeconds: number | null, locale: string): string {
  if (unixSeconds === null) return "—";
  const date = new Date(unixSeconds * 1000);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString(locale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export function formatTimestamp(value: string | null, locale: string): string {
  if (!value) return "—";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(locale, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatMiB(value: number): string {
  return `${value} MiB`;
}

export function formatSha(value: string | null): string {
  if (!value) return "—";
  return value.length > 32 ? `${value.slice(0, 32)}…` : value;
}

export function toErrorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null) {
    const candidate = (error as { message?: unknown }).message;
    if (typeof candidate === "string") return candidate;
    try {
      return JSON.stringify(error);
    } catch {
      return String(error);
    }
  }
  return String(error);
}
