import { getGitStatusOptions, listGitHubRepositoriesOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

/** Poll interval while a GitHub sign-in waits for the person to finish on the website. */
const SIGN_IN_POLL_MS = 3_000;

export const gitStatusQueryOptions = queryOptions({
  ...getGitStatusOptions(),
  staleTime: 30_000,
  refetchInterval: (query) => (query.state.data?.github.login_prompt ? SIGN_IN_POLL_MS : false),
});

/** The GitHub account's repositories, suggested when cloning. Empty while signed out. */
export const gitHubRepositoriesQueryOptions = queryOptions({
  ...listGitHubRepositoriesOptions(),
  staleTime: 60_000,
});
