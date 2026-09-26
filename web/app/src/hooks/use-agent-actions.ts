import { installAgent, logOutAgent, startAgentLogin, submitAgentLoginCode } from "@ezra/client";
import type { Agent } from "@ezra/client";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { AGENTS, INSTALL_AGENT } from "@/queries/query-keys";

/** Every action on one agent. Each refreshes the agent list when it settles. */
export function useAgentActions(agent: Agent) {
  const queryClient = useQueryClient();
  const refreshAgents = () => queryClient.invalidateQueries({ queryKey: [AGENTS] });
  const path = { agent };

  const install = useMutation({
    mutationKey: [INSTALL_AGENT, agent],
    mutationFn: async () => installAgent({ path, throwOnError: true }),
    onSettled: refreshAgents,
  });
  const startSignIn = useMutation({
    mutationFn: async () => startAgentLogin({ path, throwOnError: true }),
    onSettled: refreshAgents,
  });
  const submitCode = useMutation({
    mutationFn: async (code: string) =>
      submitAgentLoginCode({ path, body: { code }, throwOnError: true }),
    onSettled: refreshAgents,
  });
  const signOut = useMutation({
    mutationFn: async () => logOutAgent({ path, throwOnError: true }),
    onSettled: refreshAgents,
  });
  return { install, startSignIn, submitCode, signOut };
}
