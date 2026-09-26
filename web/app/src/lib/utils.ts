import type { DownloadProgress } from "@ezra/client";
import prettyBytes from "pretty-bytes";

import { APP_DESCRIPTIONS } from "@/content/app";

/** Renders a byte count in decimal units, e.g. `231 MB`. */
export function formatBytes(bytes: number): string {
  return prettyBytes(bytes);
}

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
