import type { DownloadProgress } from "@ezra/client";

import { APP_DESCRIPTIONS } from "@/content/app";

/** Whole percent of a download, when the server reported its size. */
export function downloadPercent(progress: DownloadProgress): number | undefined {
  const totalBytes = progress.total_bytes ?? 0;
  if (totalBytes === 0) {
    return undefined;
  }
  return Math.min(100, Math.floor((progress.received_bytes * 100) / totalBytes));
}

/** The manager's `{ error }` message, a code error's own message, or a generic failure. */
export function errorMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "error" in error &&
    typeof error.error === "string"
  ) {
    return error.error;
  }
  return error instanceof Error ? error.message : APP_DESCRIPTIONS.request_failed;
}

const DATE_AND_TIME = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

/** A server timestamp as a date and time in the browser's language and time zone. */
export function formatDateTime(timestamp: string | Date): string {
  return DATE_AND_TIME.format(new Date(timestamp));
}
