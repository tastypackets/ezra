import type {
  AgentStatus,
  CloneStatus,
  FolderStatus,
  GitStatus,
  RemoteControlOverview,
  SessionStatus,
} from "@ezra/client";
import type { QueryClient } from "@tanstack/react-query";

import { agentsQueryOptions } from "@/queries/agent-queries";
import { clonesQueryOptions, foldersQueryOptions } from "@/queries/folder-queries";
import { gitStatusQueryOptions } from "@/queries/git-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";
import { sessionQueryOptions } from "@/queries/session-queries";

/** What the manager writes into `index.html` for the first screen. */
interface InitialData {
  revision: number;
  session: SessionStatus;
  agents: AgentStatus[] | null;
  folders: FolderStatus[] | null;
  clones: CloneStatus[] | null;
  remote_control: RemoteControlOverview | null;
  git: GitStatus | null;
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
  if (initialData.clones) {
    queryClient.setQueryData(clonesQueryOptions.queryKey, initialData.clones, { updatedAt });
  }
  if (initialData.remote_control) {
    queryClient.setQueryData(remoteControlQueryOptions.queryKey, initialData.remote_control, {
      updatedAt,
    });
  }
  if (initialData.git) {
    queryClient.setQueryData(gitStatusQueryOptions.queryKey, initialData.git, { updatedAt });
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
    !("clones" in value) ||
    !("remote_control" in value) ||
    !("git" in value)
  ) {
    return false;
  }
  const { revision, session, agents, folders, clones, remote_control: remoteControl, git } = value;
  return (
    typeof revision === "number" &&
    typeof session === "object" &&
    session !== null &&
    "claimed" in session &&
    "authenticated" in session &&
    (agents === null || Array.isArray(agents)) &&
    (folders === null || Array.isArray(folders)) &&
    (clones === null || Array.isArray(clones)) &&
    (remoteControl === null || typeof remoteControl === "object") &&
    (git === null || typeof git === "object")
  );
}
