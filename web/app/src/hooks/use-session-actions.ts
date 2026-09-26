import { logIn, logOut, setUpPassword } from "@ezra/client";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { AGENTS } from "@/queries/query-keys";
import { sessionQueryOptions } from "@/queries/session-queries";

/** Setting the password, signing in and signing out. Each updates the cached session. */
export function useSessionActions() {
  const queryClient = useQueryClient();
  const markSignedIn = () =>
    queryClient.setQueryData(sessionQueryOptions.queryKey, { claimed: true, authenticated: true });

  const setUp = useMutation({
    mutationFn: async (password: string) =>
      setUpPassword({ body: { password }, throwOnError: true }),
    onSuccess: markSignedIn,
  });
  const signIn = useMutation({
    mutationFn: async (password: string) => logIn({ body: { password }, throwOnError: true }),
    onSuccess: markSignedIn,
  });
  const signOut = useMutation({
    mutationFn: async () => logOut({ throwOnError: true }),
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
