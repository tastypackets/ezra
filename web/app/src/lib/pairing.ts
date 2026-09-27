import type { PairedPhone } from "@ezra/client";

import { PAIRING_DESCRIPTIONS } from "@/content/pairing";

/** The time left until `expiresAt` as `m:ss`, `0:00` once it passed. */
export function countdown(expiresAt: string, now: Date): string {
  const seconds = Math.max(0, Math.ceil((Date.parse(expiresAt) - now.getTime()) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** What a phone calls itself, else its model, its platform or "Phone". */
export function phoneName(phone: PairedPhone): string {
  return (
    [phone.name, phone.model, phone.platform].find((part) => part?.trim()) ??
    PAIRING_DESCRIPTIONS.unnamed_phone
  );
}
