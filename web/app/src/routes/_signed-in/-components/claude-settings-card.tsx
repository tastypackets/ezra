import { updateClaudeSettings } from "@ezra/client";
import type { ClaudeSettingsBody, ReleaseChannel } from "@ezra/client";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { cn } from "cn";

import { Button } from "@/components/ui/button";
import { Card, CardFooter, CardHeader } from "@/components/ui/card";
import { ChoiceCards } from "@/components/ui/choice-cards";
import { AGENT_NAMES } from "@/content/agents";
import { RELEASE_CHANNELS, SETTINGS_DESCRIPTIONS } from "@/content/settings";
import { errorMessage } from "@/lib/utils";
import { AGENTS } from "@/queries/query-keys";
import { claudeSettingsQueryOptions } from "@/queries/settings-queries";

const CHANNEL_ORDER: readonly ReleaseChannel[] = ["latest", "stable"];
const CHANNEL_CHOICES = CHANNEL_ORDER.map((channel) => ({
  value: channel,
  ...RELEASE_CHANNELS[channel],
}));

/** How the manager installs and updates Claude Code. */
export function ClaudeSettingsCard() {
  const queryClient = useQueryClient();
  const { data: settings } = useSuspenseQuery(claudeSettingsQueryOptions);
  const save = useMutation({
    mutationFn: async (body: ClaudeSettingsBody) =>
      (await updateClaudeSettings({ body, throwOnError: true })).data,
    onSuccess: (saved) => {
      queryClient.setQueryData(claudeSettingsQueryOptions.queryKey, saved);
      void queryClient.invalidateQueries({ queryKey: [AGENTS] });
    },
  });
  const form = useForm({
    defaultValues: settings,
    onSubmit: async ({ value, formApi }) => {
      try {
        formApi.reset(await save.mutateAsync(value));
      } catch {
        return;
      }
    },
  });
  return (
    <Card>
      <CardHeader
        title={AGENT_NAMES.claude}
        description={SETTINGS_DESCRIPTIONS.claude_description}
      />
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!form.state.isSubmitting) {
            void form.handleSubmit();
          }
        }}
      >
        <div className="px-5 py-4">
          <form.Field name="release_channel">
            {(field) => (
              <ChoiceCards
                label={SETTINGS_DESCRIPTIONS.release_channel}
                hint={SETTINGS_DESCRIPTIONS.release_channel_hint}
                value={field.state.value}
                choices={CHANNEL_CHOICES}
                onValueChange={field.handleChange}
              />
            )}
          </form.Field>
        </div>
        <CardFooter>
          <form.Subscribe selector={(state) => state.isDefaultValue}>
            {(unchanged) => (
              <p
                role="status"
                className={cn(save.isError ? "text-[0.8125rem] text-ez-danger" : "text-ez-muted")}
              >
                {save.isError
                  ? errorMessage(save.error)
                  : save.isSuccess && unchanged
                    ? SETTINGS_DESCRIPTIONS.saved
                    : null}
              </p>
            )}
          </form.Subscribe>
          <form.Subscribe selector={(state) => state.isSubmitting}>
            {(isSubmitting) => (
              <Button type="submit" variant="primary" loading={isSubmitting}>
                {SETTINGS_DESCRIPTIONS.save}
              </Button>
            )}
          </form.Subscribe>
        </CardFooter>
      </form>
    </Card>
  );
}
