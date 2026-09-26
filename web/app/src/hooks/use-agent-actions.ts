import { installAgent, logOutAgent, startAgentLogin, submitAgentLoginCode } from "@ezra/client";
import type { Agent } from "@ezra/client";
import { useMutation, useMutationState, useQueryClient } from "@tanstack/react-query";
import type { Mutation } from "@tanstack/react-query";

import { errorMessage } from "@/lib/utils";
import { AGENT_ACTION, AGENTS } from "@/queries/query-keys";

type AgentAction = "install" | "start_sign_in" | "submit_code" | "sign_out";

/** Every action on one agent. Each refreshes the agent list when it settles. */
export function useAgentActions(agent: Agent) {
  const queryClient = useQueryClient();
  const refreshAgents = () => queryClient.invalidateQueries({ queryKey: [AGENTS] });
  const mutationKey = (action: AgentAction) => [AGENT_ACTION, agent, action];
  const path = { agent };

  const install = useMutation({
    mutationKey: mutationKey("install"),
    mutationFn: async () => installAgent({ path, throwOnError: true }),
    onSettled: refreshAgents,
  });
  const startSignIn = useMutation({
    mutationKey: mutationKey("start_sign_in"),
    mutationFn: async () => startAgentLogin({ path, throwOnError: true }),
    onSettled: refreshAgents,
  });
  const submitCode = useMutation({
    mutationKey: mutationKey("submit_code"),
    mutationFn: async (code: string) =>
      submitAgentLoginCode({ path, body: { code }, throwOnError: true }),
    onSettled: refreshAgents,
  });
  const signOut = useMutation({
    mutationKey: mutationKey("sign_out"),
    mutationFn: async () => logOutAgent({ path, throwOnError: true }),
    onSettled: refreshAgents,
  });
  return { install, startSignIn, submitCode, signOut };
}

/** Why the agent's most recent failed action failed, until that action runs again. */
export function useAgentActionError(agent: Agent): string | undefined {
  const runs = useMutationState({
    filters: { mutationKey: [AGENT_ACTION, agent] },
    select: (mutation) => ({ action: mutation.options.mutationKey?.[2], state: mutation.state }),
  });
  const newestPerAction = new Map<unknown, (typeof runs)[number]["state"]>();
  for (const { action, state } of runs) {
    const newest = newestPerAction.get(action);
    if (!newest || state.submittedAt >= newest.submittedAt) {
      newestPerAction.set(action, state);
    }
  }
  const newestFailure = [...newestPerAction.values()]
    .filter((state) => state.status === "error")
    .toSorted((first, second) => second.submittedAt - first.submittedAt)
    .at(0);
  return newestFailure ? errorMessage(newestFailure.error) : undefined;
}

export function isInstallMutation(mutation: Mutation): boolean {
  const [prefix, , action] = mutation.options.mutationKey ?? [];
  return prefix === AGENT_ACTION && action === "install";
}
