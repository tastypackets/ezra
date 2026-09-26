import type { AgentStatus } from "@ezra/client";

/** How many days ahead the page warns that a sign-in ends. */
export const SIGN_IN_WARNING_DAYS = 3;
const DAY_MS = 86_400_000;

export interface SignInEnd {
  at: Date;
  ended: boolean;
}

/** When the agent's sign-in stops working, once that is at most the warning days away. */
export function signInEnd(status: AgentStatus, now = new Date()): SignInEnd | undefined {
  if (!status.logged_in || !status.sign_in_ends_at) {
    return undefined;
  }
  const at = new Date(status.sign_in_ends_at);
  const left = at.getTime() - now.getTime();
  if (Number.isNaN(left) || left > SIGN_IN_WARNING_DAYS * DAY_MS) {
    return undefined;
  }
  return { at, ended: left <= 0 };
}
