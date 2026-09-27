import type { Agent, RemoteControlOverview, ServerState } from "@ezra/client";

import type { BadgeVariant } from "@/components/ui/badge";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";

export const SERVER_BADGES: Record<ServerState, BadgeVariant> = {
  off: "secondary",
  waiting: "secondary",
  starting: "warning",
  running: "success",
  retrying: "warning",
  stopping: "warning",
};

/** Whether any of the agent's remote control servers runs. */
export function serversRun(
  agent: Agent,
  { projects, folders, codex }: RemoteControlOverview,
): boolean {
  const servers = agent === "claude" ? [projects, ...Object.values(folders)] : [codex];
  return servers.some((server) => server.state === "running");
}

/** Remote Control in a few words: off, waiting, or its servers and their sessions. */
export function remoteControlSummary({ projects, folders }: RemoteControlOverview): string {
  if (projects.state === "off" || projects.state === "waiting") {
    return SERVER_STATES[projects.state];
  }
  const servers = [projects, ...Object.values(folders)].filter(
    (server) => server.state !== "off" && server.state !== "waiting",
  );
  const sessions = servers.reduce((total, server) => total + (server.usage?.sessions ?? 0), 0);
  return REMOTE_CONTROL_DESCRIPTIONS.summary(servers.length, sessions);
}
