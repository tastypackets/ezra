import type { AgentStatus, SessionStatus } from "@ezra/client";
import type { QueryClient } from "@tanstack/react-query";

import { agentsQueryOptions } from "@/queries/agent-queries";
import { sessionQueryOptions } from "@/queries/session-queries";

/** What the manager writes into `index.html` for the first screen. */
interface InitialData {
  session: SessionStatus;
  agents: AgentStatus[] | null;
}

/**
 * Seeds the query cache from the manager's `index.html`, so the first screen renders without a
 * request. Missing under `vite dev`, where the queries fetch as usual.
 */
export function seedInitialData(queryClient: QueryClient): void {
  const element = document.getElementById("initial-data");
  if (!element?.textContent) {
    return;
  }
  const initialData: unknown = JSON.parse(element.textContent);
  if (!isInitialData(initialData)) {
    return;
  }
  const updatedAt = Date.now();
  queryClient.setQueryData(sessionQueryOptions.queryKey, initialData.session, { updatedAt });
  if (initialData.agents) {
    queryClient.setQueryData(agentsQueryOptions.queryKey, initialData.agents, { updatedAt });
  }
  element.remove();
}

function isInitialData(value: unknown): value is InitialData {
  if (
    typeof value !== "object" ||
    value === null ||
    !("session" in value) ||
    !("agents" in value)
  ) {
    return false;
  }
  const { session, agents } = value;
  return (
    typeof session === "object" &&
    session !== null &&
    "claimed" in session &&
    "authenticated" in session &&
    (agents === null || Array.isArray(agents))
  );
}
