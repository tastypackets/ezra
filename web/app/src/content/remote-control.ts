import type { ServerProblem, ServerState } from "@ezra/client";

function sessionCount(sessions: number, capacity?: number | null): string {
  if (capacity != null) {
    return `${sessions} of ${capacity} sessions`;
  }
  return sessions === 1 ? "1 session" : `${sessions} sessions`;
}

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
  update_waiting: (version: string, time: string) =>
    `Restarts on Claude Code ${version} by ${time}.`,
  update_waiting_short: (version: string) => `Waits to restart on Claude Code ${version}.`,
  projects_log_title: "/projects log",
  log_title: (folder: string) => `${folder} log`,
  log_description: (path: string) => `The last lines of ${path}.`,
  log_empty: "Nothing logged yet.",
  log_lines: "Log lines",
  close: "Close",
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
