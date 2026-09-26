import { useNavigate } from "@tanstack/react-router";

import { Button } from "@/components/ui/button";
import { SESSION_DESCRIPTIONS } from "@/content/session";
import { useSessionActions } from "@/hooks/use-session-actions";

export interface AppHeaderProps {
  /** Shows the manager's sign-out button. */
  signedIn: boolean;
}

export function AppHeader({ signedIn }: AppHeaderProps) {
  const navigate = useNavigate();
  const { signOut } = useSessionActions();
  return (
    <header className="mb-6 flex items-center justify-between gap-4">
      <h1 className="text-lg font-semibold">{SESSION_DESCRIPTIONS.app_name}</h1>
      {signedIn ? (
        <Button
          loading={signOut.isPending}
          onClick={() =>
            signOut.mutate(undefined, { onSuccess: () => void navigate({ to: "/login" }) })
          }
        >
          {SESSION_DESCRIPTIONS.sign_out}
        </Button>
      ) : null}
    </header>
  );
}
