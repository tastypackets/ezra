import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";

import { Button } from "@/components/ui/button";
import { APP_DESCRIPTIONS } from "@/content/app";
import { SESSION_DESCRIPTIONS } from "@/content/session";
import { useSessionActions } from "@/hooks/use-session-actions";
import { errorMessage } from "@/lib/utils";
import { sessionQueryOptions } from "@/queries/session-queries";

/** The app name, plus navigation and the manager's sign-out while signed in. */
export function AppHeader() {
  const navigate = useNavigate();
  const { data: session } = useQuery(sessionQueryOptions);
  const { signOut } = useSessionActions();
  return (
    <header className="mb-6 flex flex-wrap items-center gap-x-6 gap-y-2">
      <h1 className="text-base font-semibold">{APP_DESCRIPTIONS.app_name}</h1>
      {session?.authenticated ? (
        <nav className="order-last flex w-full gap-4 sm:order-none sm:w-auto">
          <NavLink to="/" label={APP_DESCRIPTIONS.nav_agents} />
          <NavLink to="/settings" label={APP_DESCRIPTIONS.nav_settings} />
        </nav>
      ) : null}
      {session?.authenticated ? (
        <div className="ml-auto flex items-center gap-3">
          {signOut.isError ? (
            <p role="alert" className="text-destructive">
              {errorMessage(signOut.error)}
            </p>
          ) : null}
          <Button
            variant="ghost"
            size="sm"
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

function NavLink({ to, label }: { to: "/" | "/settings"; label: string }) {
  return (
    <Link
      to={to}
      activeOptions={{ exact: true }}
      className="rounded-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 data-[status=active]:font-medium data-[status=active]:text-foreground"
    >
      {label}
    </Link>
  );
}
