import type { CodexProblem, RelayState, ServerProblem, ServerState } from "@ezra/client";

function sessionCount(sessions: number, capacity?: number | null): string {
  if (capacity != null) {
    return `${sessions} of ${capacity} sessions`;
  }
  return sessions === 1 ? "1 session" : `${sessions} sessions`;
}

function chatCount(chats: number): string {
  return chats === 1 ? "1 chat" : `${chats} chats`;
}

export const CODEX_RELAY: Record<RelayState, string> = {
  connected: "Connected",
  connecting: "Connecting",
  errored: "Could not connect",
  disabled: "Paused",
};

export const REMOTE_CONTROL_DESCRIPTIONS = {
  off_hint: "Turned off in Settings.",
  waiting_hint: "Starts once Claude Code is signed in.",
  running_hint: (device: string) => `In the Claude app, pick ${device} under Remote Control.`,
  running_hint_without_device: "In the Claude app, pick this box under Remote Control.",
  last_stop: "Last stop",
  sessions: sessionCount,
  summary: (servers: number, sessions: number) =>
    `${servers === 1 ? "1 server" : `${servers} servers`}, ${sessionCount(sessions)}`,
  memory_hint: "Memory this server and its sessions use.",
  show_log: "Show log",
  sign_in_again: "Sign in again",
  update_waiting: (agent: string, version: string, time: string) =>
    `Restarts on ${agent} ${version} by ${time}.`,
  update_waiting_short: (version: string) => `Waits to restart on Claude Code ${version}.`,
  projects_log_title: "/projects log",
  log_title: (name: string) => `${name} log`,
  log_description: (path: string) => `The last lines of ${path}.`,
  log_empty: "Nothing logged yet.",
  log_lines: "Log lines",
  close: "Close",
  cancel: "Cancel",
  codex_connected: (chats: number) =>
    chats > 0 ? `${CODEX_RELAY.connected}, ${chatCount(chats)}` : CODEX_RELAY.connected,
  codex_relay_errored_note: "Codex keeps trying to reach ChatGPT.",
  codex_memory: (size: string) => `Uses ${size}`,
  codex_unsupported: (version: string) => `Codex ${version} cannot run remote control here.`,
  codex_unsupported_kept: (version: string, running: string) =>
    `Codex ${version} cannot run remote control here, so ${running} keeps running.`,
  try_again: "Try again",
  sign_in_with_chatgpt: "Sign in with ChatGPT",
  codex_sign_in_again_title: "Sign in to Codex again?",
  codex_sign_in_again_body: (chatsRunning: boolean) =>
    `The current sign-in ends at once, even if the new one does not finish.${chatsRunning ? " Running chats stop too." : ""}`,
} as const;

export const SERVER_STATES: Record<ServerState, string> = {
  off: "Off",
  waiting: "Waiting",
  starting: "Starting",
  running: "Running",
  retrying: "Restarting",
  stopping: "Stopping",
};

export const SERVER_PROBLEMS: Record<ServerProblem, string> = {
  sign_in: "Claude Code's sign-in does not work for Remote Control.",
  blocked_by_setting: "A Claude Code setting or environment variable stops Remote Control.",
  not_enabled: "Claude says Remote Control is off for this account.",
  not_allowed: "This organization does not allow Remote Control.",
  offline: "Claude's servers could not be reached or kept failing.",
};

export const CODEX_PROBLEM_LABELS: Record<CodexProblem, string> = {
  mfa_required: "Needs MFA",
  not_chatgpt: "Needs a ChatGPT sign-in",
  signed_out: "Needs a new sign-in",
  not_allowed: "Not allowed",
  socket_in_use: "Blocked",
  relay_unavailable: CODEX_RELAY.errored,
  unsupported_version: "Not supported",
};

/** The line under a problem's label, absent when its fix button says it. */
export const CODEX_PROBLEM_NOTES: Record<
  Exclude<CodexProblem, "unsupported_version">,
  string | null
> = {
  mfa_required: "Turn on multi-factor authentication in ChatGPT, then try again.",
  not_chatgpt: null,
  signed_out: null,
  not_allowed: "Codex's managed requirements turn remote control off.",
  socket_in_use: "Another Codex server was running in this box.",
  relay_unavailable: REMOTE_CONTROL_DESCRIPTIONS.codex_relay_errored_note,
};
