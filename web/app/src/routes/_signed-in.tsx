import { createFileRoute, redirect } from "@tanstack/react-router";

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
});
