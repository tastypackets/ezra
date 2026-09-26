import type { AgentStatus } from "@ezra/client";
import { listAgentsOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

/** Poll interval while a download is running, fast enough for a moving percentage. */
export const INSTALL_POLL_MS = 500;
/** Poll interval while a sign-in or a Remote Control server is waiting on something outside. */
const WAITING_POLL_MS = 3_000;
/** Poll interval while a Remote Control server waits to start again, up to minutes away. */
const RETRYING_POLL_MS = 15_000;

/** Every agent's state. Polls only while something is in progress. */
export const agentsQueryOptions = queryOptions({
  ...listAgentsOptions(),
  staleTime: 5_000,
  refetchInterval: (query) => pollInterval(query.state.data),
});

export function pollInterval(agents: AgentStatus[] | undefined): number | false {
  if (agents?.some((agent) => agent.install_progress)) {
    return INSTALL_POLL_MS;
  }
  if (
    agents?.some(
      (agent) =>
        (agent.agent === "codex" && agent.login_prompt) ||
        agent.remote_control?.state === "starting",
    )
  ) {
    return WAITING_POLL_MS;
  }
  if (agents?.some((agent) => agent.remote_control?.state === "retrying")) {
    return RETRYING_POLL_MS;
  }
  return false;
}
