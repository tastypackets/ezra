import { createFileRoute } from "@tanstack/react-router";

import { gitStatusQueryOptions } from "@/queries/git-queries";
import { claudeSettingsQueryOptions } from "@/queries/settings-queries";

import { ClaudeSettingsCard } from "./-components/claude-settings-card";
import { GitCard } from "./-components/git-card";

export const Route = createFileRoute("/_signed-in/settings")({
  loader: async ({ context }) => {
    await Promise.all([
      context.queryClient.ensureQueryData(claudeSettingsQueryOptions),
      context.queryClient.ensureQueryData(gitStatusQueryOptions),
    ]);
  },
  component: SettingsPage,
});

function SettingsPage() {
  return (
    <div className="flex flex-col gap-4">
      <ClaudeSettingsCard />
      <GitCard />
    </div>
  );
}
