import { getGitStatus } from "@ezra/client";
import { queryOptions } from "@tanstack/react-query";

import { GIT } from "./query-keys";

/** Poll interval while a GitHub sign-in waits for the person to finish on the website. */
const SIGN_IN_POLL_MS = 3_000;

export const gitStatusQueryOptions = queryOptions({
  queryKey: [GIT],
  queryFn: async () => (await getGitStatus()).data,
  refetchInterval: (query) => (query.state.data?.github.login_prompt ? SIGN_IN_POLL_MS : false),
});
