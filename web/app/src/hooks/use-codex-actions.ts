import { retryCodexRemoteControlMutation } from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { AGENT_ACTION } from "@/queries/query-keys";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

import type { AgentAction } from "./use-agent-actions";
import { useAgentActions } from "./use-agent-actions";

const PATH = { agent: "codex" } as const;

/** Trying Codex's connection to ChatGPT again, and a sign-in that asks first when it ends the current one. */
export function useCodexActions() {
  const queryClient = useQueryClient();
  const { startSignIn } = useAgentActions("codex");
  const [confirmingSignIn, setConfirmingSignIn] = useState(false);
  const retry = useMutation({
    ...retryCodexRemoteControlMutation(),
    mutationKey: [AGENT_ACTION, "codex", "retry_remote_control" satisfies AgentAction],
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: remoteControlQueryOptions.queryKey }),
  });
  const signIn = (signedIn: boolean) => {
    if (signedIn) {
      setConfirmingSignIn(true);
    } else {
      startSignIn.mutate({ path: PATH });
    }
  };
  const confirmSignIn = () => {
    setConfirmingSignIn(false);
    startSignIn.mutate({ path: PATH });
  };
  return { retry, signIn, confirmingSignIn, setConfirmingSignIn, confirmSignIn };
}
