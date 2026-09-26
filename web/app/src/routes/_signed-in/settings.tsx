import { createFileRoute } from "@tanstack/react-router";

import { agentsQueryOptions } from "@/queries/agent-queries";
import { gitStatusQueryOptions } from "@/queries/git-queries";
import { managerQueryOptions } from "@/queries/manager-queries";
import { getClaudeSettingsOptions } from "@ezra/client/react-query.gen";

import { ClaudeSettingsCard } from "./-components/claude-settings-card";
import { EnvironmentCard } from "./-components/environment-card";
import { GitCard } from "./-components/git-card";
import { ManagerCard } from "./-components/manager-card";

export const Route = createFileRoute("/_signed-in/settings")({
  loader: async ({ context }) => {
    await Promise.all([
      context.queryClient.ensureQueryData(agentsQueryOptions),
      context.queryClient.ensureQueryData(getClaudeSettingsOptions()),
      context.queryClient.ensureQueryData(gitStatusQueryOptions),
      context.queryClient.ensureQueryData(managerQueryOptions),
    ]);
  },
  component: SettingsPage,
});

function SettingsPage() {
  return (
    <div className="flex flex-col gap-4">
      <ClaudeSettingsCard />
      <GitCard />
      <ManagerCard />
      <EnvironmentCard />
    </div>
  );
}
