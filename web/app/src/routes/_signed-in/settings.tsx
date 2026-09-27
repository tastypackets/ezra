import { createFileRoute } from "@tanstack/react-router";
import { useSuspenseQuery } from "@tanstack/react-query";

import { agentsQueryOptions, isInstalled } from "@/queries/agent-queries";
import { gitStatusQueryOptions } from "@/queries/git-queries";
import { managerQueryOptions } from "@/queries/manager-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";
import { getClaudeSettingsOptions, getCodexSettingsOptions } from "@ezra/client/react-query.gen";

import { ClaudeSettingsCard } from "./-components/claude-settings-card";
import { CodexSettingsCard } from "./-components/codex-settings-card";
import { EnvironmentCard } from "./-components/environment-card";
import { GitCard } from "./-components/git-card";
import { ManagerCard } from "./-components/manager-card";

export const Route = createFileRoute("/_signed-in/settings")({
  loader: async ({ context }) => {
    await Promise.all([
      context.queryClient.ensureQueryData(agentsQueryOptions),
      context.queryClient.ensureQueryData(getClaudeSettingsOptions()),
      context.queryClient.ensureQueryData(getCodexSettingsOptions()),
      context.queryClient.ensureQueryData(gitStatusQueryOptions),
      context.queryClient.ensureQueryData(managerQueryOptions),
      context.queryClient.ensureQueryData(remoteControlQueryOptions),
    ]);
  },
  component: SettingsPage,
});

function SettingsPage() {
  const { data: codexInstalled } = useSuspenseQuery({
    ...agentsQueryOptions,
    select: isInstalled("codex"),
  });
  return (
    <div className="flex flex-col gap-4">
      <ClaudeSettingsCard />
      {codexInstalled ? <CodexSettingsCard /> : null}
      <GitCard />
      <ManagerCard />
      <EnvironmentCard />
    </div>
  );
}
