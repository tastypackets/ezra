import { useIsMutating, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";

import { isInstallMutation } from "@/hooks/use-agent-actions";
import { INSTALL_POLL_MS, agentsQueryOptions } from "@/queries/agent-queries";

import { AgentsCard } from "./-components/agents-card";
import { RemoteControlCard } from "./-components/remote-control-card";
import { SignInPanel } from "./-components/sign-in-panel";

export const Route = createFileRoute("/_signed-in/")({
  loader: async ({ context }) => {
    await context.queryClient.ensureQueryData(agentsQueryOptions);
  },
  component: DashboardPage,
});

function DashboardPage() {
  const installing = useIsMutating({ predicate: isInstallMutation }) > 0;
  const { data: agents } = useSuspenseQuery({
    ...agentsQueryOptions,
    refetchInterval: installing ? INSTALL_POLL_MS : agentsQueryOptions.refetchInterval,
  });
  return (
    <div className="flex flex-col gap-4">
      <AgentsCard agents={agents} />
      {agents.map((status) =>
        status.remote_control ? (
          <RemoteControlCard
            key={`${status.agent}-remote-control`}
            status={status.remote_control}
          />
        ) : null,
      )}
      {agents.map((status) =>
        status.login_prompt ? (
          <SignInPanel key={status.agent} agent={status.agent} prompt={status.login_prompt} />
        ) : null,
      )}
    </div>
  );
}
