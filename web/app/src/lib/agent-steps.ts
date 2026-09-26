import type { AgentStatus } from "@ezra/client";

export type AgentStep = "install" | "sign_in";

/** The action an agent's row shows as its button, absent when nothing is due. */
export function nextAgentStep(status: AgentStatus, installing: boolean): AgentStep | undefined {
  if (installing || !status.installed_version) {
    return "install";
  }
  if (!status.logged_in && !status.login_prompt) {
    return "sign_in";
  }
  return status.available_update ? "install" : undefined;
}

/**
 * Whether an installed agent's menu offers the install step, which it does unless the button
 * shows it and the menu has Sign out.
 */
export function menuOffersInstall(status: AgentStatus, step: AgentStep | undefined): boolean {
  return step !== "install" || !status.logged_in;
}
