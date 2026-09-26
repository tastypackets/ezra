import type { ServerState } from "@ezra/client";

export const REMOTE_CONTROL_DESCRIPTIONS = {
  title: "Remote Control",
  description: "Work in /projects from the Claude app and claude.ai/code.",
  open: "Open claude.ai/code",
  off_hint: "Turned off in Settings.",
  waiting_hint: "Starts once Claude Code is installed and signed in.",
  starting_hint: "Connecting to Claude.",
  running_hint: "In the Claude app, look for this box's hostname.",
  last_stop: "Last stop",
} as const;

export const SERVER_STATES: Record<ServerState, string> = {
  off: "Off",
  waiting: "Waiting",
  starting: "Starting",
  running: "Running",
  retrying: "Restarting",
};
