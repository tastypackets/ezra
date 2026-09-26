import type { AgentStatus } from "@ezra/client";
import { listAgentsOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

/** Poll interval while a download is running, fast enough for a moving percentage. */
export const INSTALL_POLL_MS = 500;

/** Every agent's state. Polls only while a download is running. */
export const agentsQueryOptions = queryOptions({
  ...listAgentsOptions(),
  staleTime: 30_000,
  refetchInterval: (query) => pollInterval(query.state.data),
});

export function pollInterval(agents: AgentStatus[] | undefined): number | false {
  return agents?.some((agent) => agent.install_progress) ? INSTALL_POLL_MS : false;
}
