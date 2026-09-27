import type { AgentStatus } from "@ezra/client";

import type { CodexFix } from "./codex-remote";

export type AgentStep = "install" | "sign_in" | CodexFix;

/**
 * The action an agent's row shows as its button, absent when nothing is due. A sign-in that is
 * about to end asks for a new one. A remote control fix comes before an update, which the menu
 * then offers.
 */
export function nextAgentStep(
  status: AgentStatus,
  installing: boolean,
  signInEnding = false,
  fix?: CodexFix,
): AgentStep | undefined {
  if (installing || !status.installed_version) {
    return "install";
  }
  if (status.login_prompt) {
    return status.available_update ? "install" : undefined;
  }
  if (!status.logged_in || signInEnding) {
    return "sign_in";
  }
  return fix ?? (status.available_update ? "install" : undefined);
}

/**
 * Whether an installed agent's menu offers the install step, which it does unless the button
 * shows it and the menu has Sign out.
 */
export function menuOffersInstall(status: AgentStatus, step: AgentStep | undefined): boolean {
  return step !== "install" || !status.logged_in;
}
