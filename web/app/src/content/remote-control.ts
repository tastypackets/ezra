import type { ServerProblem, ServerState } from "@ezra/client";

function sessionCount(sessions: number, capacity: number | null | undefined): string {
  if (capacity != null) {
    return `${sessions} of ${capacity} sessions`;
  }
  return sessions === 1 ? "1 session" : `${sessions} sessions`;
}

export const REMOTE_CONTROL_DESCRIPTIONS = {
  title: "Remote Control",
  open: "Open claude.ai/code",
  off_hint: "Turned off in Settings.",
  waiting_hint: "Starts once Claude Code is installed and signed in.",
  starting_hint: "Connecting to Claude.",
  running_hint: (device: string) => `In the Claude app, open ${device}.`,
  running_hint_without_device: "In the Claude app, open this box.",
  last_stop: "Last stop",
  usage: (sessions: number, capacity: number | null | undefined, memory: string) =>
    `${sessionCount(sessions, capacity)} running, using ${memory} of memory.`,
  sessions: (sessions: number, capacity: number | null | undefined) =>
    sessionCount(sessions, capacity),
  memory_hint: "Memory this server and its sessions use.",
  more_actions: "More Remote Control actions",
  show_log: "Show log",
  sign_in_again: "Sign in again",
  update_waiting: (version: string, time: string) =>
    `Restarts on Claude Code ${version} once no sessions run, by ${time} at the latest.`,
  update_waiting_short: (version: string) =>
    `Restarts on Claude Code ${version} once no sessions run.`,
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
  not_enabled: "Claude says Remote Control is off for this account.",
  not_allowed: "This organization does not allow Remote Control.",
  offline: "Claude's servers could not be reached or kept failing.",
};
