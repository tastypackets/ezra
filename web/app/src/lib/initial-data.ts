import type { AgentStatus, FolderStatus, RemoteControlOverview, SessionStatus } from "@ezra/client";
import type { QueryClient } from "@tanstack/react-query";

import { agentsQueryOptions } from "@/queries/agent-queries";
import { foldersQueryOptions } from "@/queries/folder-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";
import { sessionQueryOptions } from "@/queries/session-queries";

/** What the manager writes into `index.html` for the first screen. */
interface InitialData {
  revision: number;
  session: SessionStatus;
  agents: AgentStatus[] | null;
  folders: FolderStatus[] | null;
  remote_control: RemoteControlOverview | null;
}

/**
 * Seeds the query cache from the manager's `index.html`, so the first screen renders without a
 * request, and returns the event revision the data is from. Missing under `vite dev`, where the
 * queries fetch as usual.
 */
export function seedInitialData(queryClient: QueryClient): number | undefined {
  const element = document.getElementById("initial-data");
  if (!element?.textContent) {
    return undefined;
  }
  const initialData: unknown = JSON.parse(element.textContent);
  if (!isInitialData(initialData)) {
    return undefined;
  }
  const updatedAt = Date.now();
  queryClient.setQueryData(sessionQueryOptions.queryKey, initialData.session, { updatedAt });
  if (initialData.agents) {
    queryClient.setQueryData(agentsQueryOptions.queryKey, initialData.agents, { updatedAt });
  }
  if (initialData.folders) {
    queryClient.setQueryData(foldersQueryOptions.queryKey, initialData.folders, { updatedAt });
  }
  if (initialData.remote_control) {
    queryClient.setQueryData(remoteControlQueryOptions.queryKey, initialData.remote_control, {
      updatedAt,
    });
  }
  element.remove();
  return initialData.revision;
}

export function isInitialData(value: unknown): value is InitialData {
  if (
    typeof value !== "object" ||
    value === null ||
    !("revision" in value) ||
    !("session" in value) ||
    !("agents" in value) ||
    !("folders" in value) ||
    !("remote_control" in value)
  ) {
    return false;
  }
  const { revision, session, agents, folders, remote_control: remoteControl } = value;
  return (
    typeof revision === "number" &&
    typeof session === "object" &&
    session !== null &&
    "claimed" in session &&
    "authenticated" in session &&
    (agents === null || Array.isArray(agents)) &&
    (folders === null || Array.isArray(folders)) &&
    (remoteControl === null || typeof remoteControl === "object")
  );
}
