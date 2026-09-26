import { logInMutation, logOutMutation, setUpPasswordMutation } from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";

import { agentsQueryOptions } from "@/queries/agent-queries";
import { sessionQueryOptions } from "@/queries/session-queries";

/** Setting the password, signing in and signing out. Each updates the cached session. */
export function useSessionActions() {
  const queryClient = useQueryClient();
  const router = useRouter();
  const markSignedIn = () =>
    queryClient.setQueryData(sessionQueryOptions.queryKey, { claimed: true, authenticated: true });

  const setUp = useMutation({
    ...setUpPasswordMutation(),
    onSuccess: markSignedIn,
    onError: async () => {
      await queryClient.refetchQueries({ queryKey: sessionQueryOptions.queryKey });
      await router.invalidate();
    },
  });
  const signIn = useMutation({
    ...logInMutation(),
    onSuccess: markSignedIn,
  });
  const signOut = useMutation({
    ...logOutMutation(),
    onSuccess: () => {
      queryClient.setQueryData(sessionQueryOptions.queryKey, {
        claimed: true,
        authenticated: false,
      });
      queryClient.removeQueries({ queryKey: agentsQueryOptions.queryKey });
    },
  });
  return { setUp, signIn, signOut };
}
