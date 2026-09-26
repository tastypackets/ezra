import { getClaudeSettings } from "@ezra/client";
import { queryOptions } from "@tanstack/react-query";

import { CLAUDE_SETTINGS } from "./query-keys";

export const claudeSettingsQueryOptions = queryOptions({
  queryKey: [CLAUDE_SETTINGS],
  queryFn: async () => (await getClaudeSettings({ throwOnError: true })).data,
});
