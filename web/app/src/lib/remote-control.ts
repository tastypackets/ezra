import type { RemoteControlOverview, ServerState } from "@ezra/client";

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
