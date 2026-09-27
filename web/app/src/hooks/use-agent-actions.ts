import type { Agent, LoginPrompt } from "@ezra/client";
import {
  installAgentMutation,
  logOutAgentMutation,
  restartAgentServersMutation,
  startAgentLoginMutation,
  submitAgentLoginCodeMutation,
  uninstallAgentMutation,
} from "@ezra/client/react-query.gen";
import {
  useIsMutating,
  useMutation,
  useMutationState,
  useQueryClient,
} from "@tanstack/react-query";
import type { Mutation } from "@tanstack/react-query";

import { toast } from "@/components/ui/toast";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import { errorMessage } from "@/lib/utils";
import { agentsQueryOptions } from "@/queries/agent-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";
import { AGENT_ACTION } from "@/queries/query-keys";

export type AgentAction =
  | "install"
  | "start_sign_in"
  | "submit_code"
  | "sign_out"
  | "uninstall"
  | "restart_servers"
  | "retry_remote_control";

/** Every action on one agent. Each refreshes the agent list when it settles. */
export function useAgentActions(agent: Agent) {
  const queryClient = useQueryClient();
  const refreshAgents = () =>
    queryClient.invalidateQueries({ queryKey: agentsQueryOptions.queryKey });
  const mutationKey = (action: AgentAction) => [AGENT_ACTION, agent, action];

  const install = useMutation({
    ...installAgentMutation(),
    mutationKey: mutationKey("install"),
    onMutate: () =>
      queryClient
        .getQueryData(agentsQueryOptions.queryKey)
        ?.find((status) => status.agent === agent)?.installed_version,
    onSuccess: (status, _variables, versionBefore) => {
      const name = AGENT_NAMES[agent];
      if (typeof versionBefore !== "string" || !status.installed_version) {
        return;
      }
      toast.add({
        title:
          status.installed_version === versionBefore
            ? AGENTS_DESCRIPTIONS.up_to_date(name)
            : AGENTS_DESCRIPTIONS.updated(name, status.installed_version),
      });
    },
    onSettled: refreshAgents,
  });
  const startSignIn = useMutation({
    ...startAgentLoginMutation(),
    mutationKey: mutationKey("start_sign_in"),
    onSettled: refreshAgents,
  });
  const submitCode = useMutation({
    ...submitAgentLoginCodeMutation(),
    mutationKey: mutationKey("submit_code"),
    onSettled: refreshAgents,
  });
  const signOut = useMutation({
    ...logOutAgentMutation(),
    mutationKey: mutationKey("sign_out"),
    onSettled: refreshAgents,
  });
  const uninstall = useMutation({
    ...uninstallAgentMutation(),
    mutationKey: mutationKey("uninstall"),
    onSuccess: () => {
      toast.add({ title: AGENTS_DESCRIPTIONS.uninstalled(AGENT_NAMES[agent]) });
    },
    onSettled: refreshAgents,
  });
  const restartServers = useMutation({
    ...restartAgentServersMutation(),
    mutationKey: mutationKey("restart_servers"),
    onSuccess: () => {
      toast.add({ title: AGENTS_DESCRIPTIONS.restarting[agent] });
    },
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: remoteControlQueryOptions.queryKey }),
  });
  return { install, startSignIn, submitCode, signOut, uninstall, restartServers };
}

/** Whether the action is running for the agent, from any component. */
export function useAgentActionPending(agent: Agent, action: AgentAction): boolean {
  return useIsMutating({ mutationKey: [AGENT_ACTION, agent, action] }) > 0;
}

/**
 * Why the agent's most recent failed action failed, until that action runs again. Failures of
 * `hidden` are left out.
 */
export function useAgentActionError(agent: Agent, hidden?: AgentAction): string | undefined {
  const runs = useMutationState({
    filters: { mutationKey: [AGENT_ACTION, agent] },
    select: (mutation) => ({ action: mutation.options.mutationKey?.[2], state: mutation.state }),
  });
  const newestPerAction = new Map<unknown, (typeof runs)[number]["state"]>();
  for (const { action, state } of runs) {
    const newest = newestPerAction.get(action);
    if (action !== hidden && (!newest || state.submittedAt >= newest.submittedAt)) {
      newestPerAction.set(action, state);
    }
  }
  const newestFailure = [...newestPerAction.values()]
    .filter((state) => state.status === "error")
    .toSorted((first, second) => second.submittedAt - first.submittedAt)
    .at(0);
  return newestFailure ? errorMessage(newestFailure.error) : undefined;
}

/** Whether a sign-in started on this page, not elsewhere, shows the prompt. */
export function useSignInStartedHere(agent: Agent, prompt: LoginPrompt): boolean {
  return useMutationState({
    filters: { mutationKey: [AGENT_ACTION, agent, "start_sign_in"] },
    select: ({ state }) =>
      state.status === "pending" ||
      (typeof state.data === "object" &&
        state.data !== null &&
        "url" in state.data &&
        state.data.url === prompt.url),
  }).includes(true);
}

export function isInstallMutation(mutation: Mutation): boolean {
  const [prefix, , action] = mutation.options.mutationKey ?? [];
  return prefix === AGENT_ACTION && action === "install";
}
