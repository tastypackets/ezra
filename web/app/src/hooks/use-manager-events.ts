import { streamEvents } from "@ezra/client";
import type { Topic } from "@ezra/client";
import { getClaudeSettingsOptions } from "@ezra/client/react-query.gen";
import type { QueryKey } from "@tanstack/react-query";
import { useEffect } from "react";

import { queryClient } from "@/lib/query-client";
import { agentsQueryOptions } from "@/queries/agent-queries";
import { foldersQueryOptions } from "@/queries/folder-queries";
import { gitStatusQueryOptions } from "@/queries/git-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

/** First wait before reconnecting, doubled after each attempt that gets no event. */
const FIRST_RECONNECT_DELAY_MS = 1_000;
const LONGEST_RECONNECT_DELAY_MS = 30_000;

const TOPIC_QUERIES: Record<Topic, QueryKey> = {
  agents: agentsQueryOptions.queryKey,
  claude_settings: getClaudeSettingsOptions().queryKey,
  folders: foldersQueryOptions.queryKey,
  remote_control: remoteControlQueryOptions.queryKey,
  git: gitStatusQueryOptions.queryKey,
};

/**
 * Refetches what the manager says changed. On connecting, refetches everything when the data is
 * from an older revision, such as after the manager restarted.
 */
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
            void queryClient.invalidateQueries({ queryKey: TOPIC_QUERIES[event.topic] });
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
