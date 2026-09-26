import type { ServerProblem, ServerState } from "@ezra/client";

export const REMOTE_CONTROL_DESCRIPTIONS = {
  title: "Remote Control",
  open: "Open claude.ai/code",
  off_hint: "Turned off in Settings.",
  waiting_hint: "Starts once Claude Code is installed and signed in.",
  starting_hint: "Connecting to Claude.",
  running_hint: (device: string) => `In the Claude app, open ${device}.`,
  running_hint_without_device: "In the Claude app, open this box.",
  last_stop: "Last stop",
  usage: (sessions: number, capacity: number, memory: string) =>
    `${sessions} of ${capacity} sessions running, using ${memory} of memory.`,
  sessions: (sessions: number, capacity: number) => `${sessions} of ${capacity} sessions`,
  memory_hint: "Memory this server and its sessions use.",
  more_actions: "More Remote Control actions",
  show_log: "Show log",
  sign_in_again: "Sign in again",
  update_waiting: (version: string, time: string) =>
    `Restarts on Claude Code ${version} once no sessions run, by ${time} at the latest.`,
  update_waiting_short: (version: string) =>
    `Restarts on Claude Code ${version} once no sessions run.`,
  log_title: (server: string) => `${server} log`,
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
  not_allowed: "This account's plan or organization does not allow Remote Control.",
  offline: "Claude's servers cannot be reached from this box.",
  unavailable: "Claude's servers answered with errors.",
};
