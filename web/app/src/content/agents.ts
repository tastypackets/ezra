import type { Agent } from "@ezra/client";

export const AGENT_NAMES: Record<Agent, string> = {
  claude: "Claude Code",
  codex: "Codex",
};

export const AGENTS_DESCRIPTIONS = {
  title: "Agents",
  column_agent: "Agent",
  column_status: "Status",
  column_version: "Version",
  column_account: "Account",
  column_sessions: "Sessions",
  column_saved_data: "Saved data",
  column_actions: "Actions",
  saved_data_hint: "Sign-in, settings and sessions the CLI keeps on /config, not the CLI itself",
  unavailable: "unavailable",
  status_not_installed: "Not installed",
  status_signed_out: "Signed out",
  status_signing_in: "Signing in",
  status_signed_in: "Signed in",
  install: "Install",
  update_to: (version: string) => `Update to ${version}`,
  check_for_update: "Check for update",
  up_to_date: (agent: string) => `${agent} is up to date.`,
  updated: (agent: string, version: string) => `${agent} ${version} is installed.`,
  more_actions: (agent: string) => `More ${agent} actions`,
  sign_in: "Sign in",
  sign_out: "Sign out",
  sign_in_title: (agent: string) => `Sign in to ${agent}`,
  sign_in_description: "Finish these steps in any browser.",
  code_placeholder: "Code",
  code_label: "Code",
  finish_sign_in: "Finish sign-in",
  start_over: "Start over",
} as const;
