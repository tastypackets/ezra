import type { RemoteControlOverview } from "@ezra/client";

import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";

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
