import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";

import { Button } from "@/components/ui/button";
import { APP_DESCRIPTIONS } from "@/content/app";
import { SESSION_DESCRIPTIONS } from "@/content/session";
import { useSessionActions } from "@/hooks/use-session-actions";
import { errorMessage } from "@/lib/utils";
import { sessionQueryOptions } from "@/queries/session-queries";

/** The app name, plus the manager's sign-out while signed in. */
export function AppHeader() {
  const navigate = useNavigate();
  const { data: session } = useQuery(sessionQueryOptions);
  const { signOut } = useSessionActions();
  return (
    <header className="mb-6 flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
      <h1 className="text-lg font-semibold">{APP_DESCRIPTIONS.app_name}</h1>
      {session?.authenticated ? (
        <div className="flex items-center gap-3">
          {signOut.isError ? (
            <p role="alert" className="text-[0.8125rem] text-ez-danger">
              {errorMessage(signOut.error)}
            </p>
          ) : null}
          <Button
            loading={signOut.isPending}
            onClick={() =>
              signOut.mutate(undefined, { onSuccess: () => navigate({ to: "/login" }) })
            }
          >
            {SESSION_DESCRIPTIONS.sign_out}
          </Button>
        </div>
      ) : null}
    </header>
  );
}
