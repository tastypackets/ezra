import { Outlet, createFileRoute, redirect } from "@tanstack/react-router";

import { useCodexPairingClaim } from "@/hooks/use-codex-actions";
import { useManagerEvents } from "@/hooks/use-manager-events";
import { sessionQueryOptions } from "@/queries/session-queries";

export const Route = createFileRoute("/_signed-in")({
  beforeLoad: async ({ context }) => {
    const session = await context.queryClient.ensureQueryData(sessionQueryOptions);
    if (!session.claimed) {
      throw redirect({ to: "/setup" });
    }
    if (!session.authenticated) {
      throw redirect({ to: "/login" });
    }
  },
  component: SignedInLayout,
});

function SignedInLayout() {
  const { initialRevision } = Route.useRouteContext();
  useManagerEvents(initialRevision);
  useCodexPairingClaim();
  return <Outlet />;
}
