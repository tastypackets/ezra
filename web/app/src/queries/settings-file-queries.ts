import type { Agent } from "@ezra/client";
import { getSettingsFileOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

/** An agent's own settings file, refetched when the manager sees it change. */
export function settingsFileQueryOptions(agent: Agent) {
  return queryOptions({
    ...getSettingsFileOptions({ path: { agent } }),
    staleTime: 30_000,
    retry: false,
  });
}
