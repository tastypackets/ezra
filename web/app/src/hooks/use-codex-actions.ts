import type { CodexPairing } from "@ezra/client";
import {
  removeCodexPhoneMutation,
  retryCodexRemoteControlMutation,
  startCodexPairingMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import { toast } from "@/components/ui/toast";
import { PAIRING_DESCRIPTIONS } from "@/content/pairing";
import { AGENT_ACTION } from "@/queries/query-keys";
import {
  codexPairingQueryOptions,
  codexPairingWatchQueryOptions,
  codexPhonesQueryOptions,
  remoteControlQueryOptions,
} from "@/queries/remote-control-queries";

import type { AgentAction } from "./use-agent-actions";
import { useAgentActions } from "./use-agent-actions";

const PATH = { agent: "codex" } as const;

/**
 * Trying Codex's connection to ChatGPT again, a sign-in that asks first when it ends the current
 * one, and removing a paired phone.
 */
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
  const removePhone = useMutation({
    ...removeCodexPhoneMutation(),
    onSuccess: (_, { path }) => {
      queryClient.setQueryData(codexPhonesQueryOptions.queryKey, (phones) =>
        phones?.filter((phone) => phone.id !== path.id),
      );
      toast.add({ title: PAIRING_DESCRIPTIONS.removed });
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: codexPhonesQueryOptions.queryKey }),
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
  return { retry, removePhone, signIn, confirmingSignIn, setConfirmingSignIn, confirmSignIn };
}

/**
 * Pairing a phone from Codex's row: the dialog, the code it shows, and whether it opened while
 * Codex could not pair and no code was asked for since.
 */
export function useCodexPairing() {
  const queryClient = useQueryClient();
  const [openAsked, setOpen] = useState(false);
  const [openedUnpairable, setOpenedUnpairable] = useState(false);
  const watch = (pairing: CodexPairing) => {
    queryClient.setQueryData(codexPairingQueryOptions.queryKey, { pairing });
    queryClient.setQueryData(codexPairingWatchQueryOptions.queryKey, true);
  };
  const start = useMutation({ ...startCodexPairingMutation(), onSuccess: watch });
  const { data } = useQuery({ ...codexPairingQueryOptions, enabled: false });
  const unpaired = openedUnpairable && start.isIdle;
  const pairing = unpaired ? undefined : (data?.pairing ?? undefined);
  const claimed = (start.isIdle || start.isSuccess) && pairing?.state === "claimed";
  /** Opens the dialog on the open code or a new one, or on why Codex cannot pair. */
  const pairPhone = async (pairable: boolean) => {
    start.reset();
    setOpen(false);
    setOpenedUnpairable(!pairable);
    if (!pairable) {
      setOpen(true);
      return;
    }
    const latest = await queryClient.fetchQuery(codexPairingQueryOptions).catch(() => undefined);
    if (latest?.pairing?.state === "open") {
      watch(latest.pairing);
      setOpen(true);
    } else {
      start.mutate({}, { onSettled: () => setOpen(true) });
    }
  };
  return {
    open: openAsked && !claimed,
    setOpen,
    unpaired,
    pairing,
    start,
    pairPhone,
  };
}

/** Polls the code this tab watches while it is open, and shows one toast once a phone used it. */
export function useCodexPairingClaim() {
  const queryClient = useQueryClient();
  const { data: watching = false } = useQuery(codexPairingWatchQueryOptions);
  const { data: state } = useQuery({
    ...codexPairingQueryOptions,
    enabled: watching,
    select: ({ pairing }) => pairing?.state,
  });
  useEffect(() => {
    if (!watching || state === "open") {
      return;
    }
    if (state === "claimed") {
      toast.add({ title: PAIRING_DESCRIPTIONS.paired });
    }
    queryClient.setQueryData(codexPairingWatchQueryOptions.queryKey, false);
  }, [queryClient, watching, state]);
}

export type CodexPairingFlow = ReturnType<typeof useCodexPairing>;
