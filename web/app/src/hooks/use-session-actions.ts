import { logIn, logOut, setUpPassword } from "@ezra/client";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";

import { AGENTS } from "@/queries/query-keys";
import { sessionQueryOptions } from "@/queries/session-queries";

/** Setting the password, signing in and signing out. Each updates the cached session. */
export function useSessionActions() {
  const queryClient = useQueryClient();
  const router = useRouter();
  const markSignedIn = () =>
    queryClient.setQueryData(sessionQueryOptions.queryKey, { claimed: true, authenticated: true });

  const setUp = useMutation({
    mutationFn: async (password: string) => setUpPassword({ body: { password } }),
    onSuccess: markSignedIn,
    onError: async () => {
      await queryClient.refetchQueries({ queryKey: sessionQueryOptions.queryKey });
      await router.invalidate();
    },
  });
  const signIn = useMutation({
    mutationFn: async (password: string) => logIn({ body: { password } }),
    onSuccess: markSignedIn,
  });
  const signOut = useMutation({
    mutationFn: async () => logOut(),
    onSuccess: () => {
      queryClient.setQueryData(sessionQueryOptions.queryKey, {
        claimed: true,
        authenticated: false,
      });
      queryClient.removeQueries({ queryKey: [AGENTS] });
    },
  });
  return { setUp, signIn, signOut };
}
