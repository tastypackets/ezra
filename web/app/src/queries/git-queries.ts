import { getGitStatusOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

/** Poll interval while a GitHub sign-in waits for the person to finish on the website. */
const SIGN_IN_POLL_MS = 3_000;

export const gitStatusQueryOptions = queryOptions({
  ...getGitStatusOptions(),
  refetchInterval: (query) => (query.state.data?.github.login_prompt ? SIGN_IN_POLL_MS : false),
});
