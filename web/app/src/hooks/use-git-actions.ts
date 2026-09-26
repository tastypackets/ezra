import { logOutOfGitHub, startGitHubLogin, updateCommitIdentity } from "@ezra/client";
import type { CommitIdentity, GitStatus } from "@ezra/client";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toastManager } from "@/components/ui/toast";
import { GIT_DESCRIPTIONS } from "@/content/git";
import { gitStatusQueryOptions } from "@/queries/git-queries";

/** GitHub sign-in and out, and the commit identity. Each refreshes the git status. */
export function useGitActions() {
  const queryClient = useQueryClient();
  const refreshGit = () =>
    queryClient.invalidateQueries({ queryKey: gitStatusQueryOptions.queryKey });

  const startGitHubSignIn = useMutation({
    mutationFn: async () => startGitHubLogin({ throwOnError: true }),
    onSettled: refreshGit,
  });
  const signOutOfGitHub = useMutation({
    mutationFn: async () => logOutOfGitHub({ throwOnError: true }),
    onSettled: refreshGit,
  });
  const saveIdentity = useMutation({
    mutationFn: async (identity: CommitIdentity) =>
      (await updateCommitIdentity({ body: identity, throwOnError: true })).data,
    onMutate: () => queryClient.cancelQueries({ queryKey: gitStatusQueryOptions.queryKey }),
    onSuccess: (identity) => {
      queryClient.setQueryData(gitStatusQueryOptions.queryKey, (status: GitStatus | undefined) =>
        status ? { ...status, identity } : status,
      );
      toastManager.add({ title: GIT_DESCRIPTIONS.saved });
    },
  });
  return { startGitHubSignIn, signOutOfGitHub, saveIdentity };
}
