import type { GetCodexPairingQrData } from "@ezra/client";
import { client } from "@ezra/client/client.gen";
import {
  getCodexPairingOptions,
  getRemoteControlOptions,
  listCodexPhonesOptions,
  listCodexModelsOptions,
} from "@ezra/client/react-query.gen";
import { queryOptions, skipToken } from "@tanstack/react-query";

/** Poll interval while a pairing code waits for a phone. */
const PAIRING_POLL_MS = 2_000;
const PAIRING_QR: GetCodexPairingQrData["url"] = "/api/v1/remote-control/codex/pairing/qr.svg";

export const remoteControlQueryOptions = queryOptions({
  ...getRemoteControlOptions(),
  staleTime: 30_000,
});

/** The pairing code asked for last, polled while a phone can still use it. */
export const codexPairingQueryOptions = queryOptions({
  ...getCodexPairingOptions(),
  staleTime: PAIRING_POLL_MS,
  refetchInterval: (query) =>
    query.state.data?.pairing?.state === "open" ? PAIRING_POLL_MS : false,
});

/** Whether this tab asked for or showed the pairing code and waits for a phone to use it. */
export const codexPairingWatchQueryOptions = queryOptions<boolean>({
  queryKey: ["codex_pairing_watch"],
  queryFn: skipToken,
});

/** The phones paired with Codex, fetched each time they are shown. */
export const codexPhonesQueryOptions = queryOptions({
  ...listCodexPhonesOptions(),
  staleTime: 0,
  retry: false,
});

export const codexModelsQueryOptions = queryOptions({
  ...listCodexModelsOptions(),
  staleTime: 60_000,
  retry: false,
});

/** The QR code of the pairing code that expires at `expiresAt`. */
export function codexPairingQrUrl(expiresAt: string): string {
  return client.buildUrl({ url: PAIRING_QR, query: { v: expiresAt } });
}
