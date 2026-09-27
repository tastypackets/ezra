import type { GitStatus } from "@ezra/client";
import {
  logOutOfGitHubMutation,
  startGitHubLoginMutation,
  updateCommitIdentityMutation,
} from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toast } from "@/components/ui/toast";
import { GIT_DESCRIPTIONS } from "@/content/git";
import { gitStatusQueryOptions } from "@/queries/git-queries";

/** GitHub sign-in and out, and the commit identity. Each refreshes the git status. */
export function useGitActions() {
  const queryClient = useQueryClient();
  const refreshGit = () =>
    queryClient.invalidateQueries({ queryKey: gitStatusQueryOptions.queryKey });

  const startGitHubSignIn = useMutation({
    ...startGitHubLoginMutation(),
    onSettled: refreshGit,
  });
  const signOutOfGitHub = useMutation({
    ...logOutOfGitHubMutation(),
    onSettled: refreshGit,
  });
  const saveIdentity = useMutation({
    ...updateCommitIdentityMutation(),
    onMutate: () => queryClient.cancelQueries({ queryKey: gitStatusQueryOptions.queryKey }),
    onSuccess: (identity) => {
      queryClient.setQueryData(gitStatusQueryOptions.queryKey, (status: GitStatus | undefined) =>
        status ? { ...status, identity } : status,
      );
      toast.add({ title: GIT_DESCRIPTIONS.saved });
    },
  });
  return { startGitHubSignIn, signOutOfGitHub, saveIdentity };
}
