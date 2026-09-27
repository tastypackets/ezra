import { streamEvents } from "@ezra/client";
import type { Topic } from "@ezra/client";
import { getClaudeSettingsOptions, getCodexSettingsOptions } from "@ezra/client/react-query.gen";
import type { QueryKey } from "@tanstack/react-query";
import { useEffect } from "react";

import { queryClient } from "@/lib/query-client";
import { agentsQueryOptions } from "@/queries/agent-queries";
import { clonesQueryOptions, foldersQueryOptions } from "@/queries/folder-queries";
import { gitHubRepositoriesQueryOptions, gitStatusQueryOptions } from "@/queries/git-queries";
import { managerQueryOptions } from "@/queries/manager-queries";
import {
  codexPhonesQueryOptions,
  remoteControlQueryOptions,
} from "@/queries/remote-control-queries";

/** First wait before reconnecting, doubled after each attempt that gets no event. */
const FIRST_RECONNECT_DELAY_MS = 1_000;
const LONGEST_RECONNECT_DELAY_MS = 30_000;

const TOPIC_QUERIES: Record<Topic, readonly QueryKey[]> = {
  agents: [agentsQueryOptions.queryKey],
  claude_settings: [getClaudeSettingsOptions().queryKey],
  codex_settings: [getCodexSettingsOptions().queryKey],
  codex_phones: [codexPhonesQueryOptions.queryKey],
  folders: [foldersQueryOptions.queryKey],
  clones: [clonesQueryOptions.queryKey],
  remote_control: [remoteControlQueryOptions.queryKey],
  git: [gitStatusQueryOptions.queryKey, gitHubRepositoriesQueryOptions.queryKey],
  manager: [managerQueryOptions.queryKey],
};

/** Refetches what the manager says changed, and everything when a connection finds a newer revision. */
export function useManagerEvents(initialRevision: number | undefined) {
  useEffect(() => {
    const controller = new AbortController();
    let revision = initialRevision;
    void (async () => {
      let delay = FIRST_RECONNECT_DELAY_MS;
      while (!controller.signal.aborted) {
        const { stream } = await streamEvents({
          signal: controller.signal,
          sseMaxRetryAttempts: 1,
        });
        for await (const event of stream) {
          delay = FIRST_RECONNECT_DELAY_MS;
          if (event.event === "changed") {
            for (const queryKey of TOPIC_QUERIES[event.topic]) {
              void queryClient.invalidateQueries({ queryKey });
            }
          } else if (event.revision !== revision) {
            void queryClient.invalidateQueries();
          }
          revision = event.revision;
        }
        await new Promise((resolve) => setTimeout(resolve, delay));
        delay = Math.min(delay * 2, LONGEST_RECONNECT_DELAY_MS);
        if (!controller.signal.aborted) {
          // A plain request, so an ended session goes to sign-in.
          await queryClient
            .fetchQuery({ ...remoteControlQueryOptions, staleTime: 0, retry: false })
            .catch(() => undefined);
        }
      }
    })();
    return () => controller.abort();
  }, [initialRevision]);
}
