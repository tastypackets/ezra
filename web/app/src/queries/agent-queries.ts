import { listAgents } from "@ezra/client";
import type { AgentStatus } from "@ezra/client";
import { queryOptions } from "@tanstack/react-query";

import { AGENTS } from "./query-keys";

/** Poll interval while a download is running, fast enough for a moving percentage. */
export const INSTALL_POLL_MS = 500;
/** Poll interval while a Codex sign-in waits for the person to finish on the website. */
const SIGN_IN_POLL_MS = 3_000;

/** Every agent's state. Polls only while something is in progress. */
export const agentsQueryOptions = queryOptions({
  queryKey: [AGENTS],
  queryFn: async () => (await listAgents({ throwOnError: true })).data,
  staleTime: 5_000,
  refetchInterval: (query) => pollInterval(query.state.data),
});

function pollInterval(agents: AgentStatus[] | undefined): number | false {
  if (agents?.some((agent) => agent.install_progress)) {
    return INSTALL_POLL_MS;
  }
  if (agents?.some((agent) => agent.agent === "codex" && agent.login_prompt)) {
    return SIGN_IN_POLL_MS;
  }
  return false;
}
