import { updateClaudeSettings } from "@ezra/client";
import type { ClaudeSettingsBody, ReleaseChannel } from "@ezra/client";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { cn } from "cn";

import { Button } from "@/components/ui/button";
import { Card, CardFooter, CardHeader } from "@/components/ui/card";
import { ChoiceCards } from "@/components/ui/choice-cards";
import { Combobox } from "@/components/ui/combobox";
import { Field, TextInput } from "@/components/ui/field";
import { Switch } from "@/components/ui/switch";
import { AGENT_NAMES } from "@/content/agents";
import { PERMISSION_MODES, RELEASE_CHANNELS, SETTINGS_DESCRIPTIONS } from "@/content/settings";
import { errorMessage } from "@/lib/utils";
import { AGENTS } from "@/queries/query-keys";
import { claudeSettingsQueryOptions } from "@/queries/settings-queries";

const CHANNEL_ORDER: readonly ReleaseChannel[] = ["latest", "stable"];
const CHANNEL_CHOICES = CHANNEL_ORDER.map((channel) => ({
  value: channel,
  ...RELEASE_CHANNELS[channel],
}));

/** How the manager installs, updates and serves Claude Code. */
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
        <div className="flex flex-col gap-4 border-b border-ez-border px-5 py-4">
          <h3 className="font-medium">{SETTINGS_DESCRIPTIONS.remote_control}</h3>
          <form.Field name="remote_control.enabled">
            {(field) => (
              <Switch
                label={SETTINGS_DESCRIPTIONS.remote_control_enabled}
                description={SETTINGS_DESCRIPTIONS.remote_control_enabled_hint}
                checked={field.state.value}
                onCheckedChange={field.handleChange}
              />
            )}
          </form.Field>
          <div className="grid gap-3 sm:grid-cols-2">
            <form.Field
              name="remote_control.permission_mode"
              validators={{
                onChange: ({ value }) =>
                  /^\S+$/.test(value.trim())
                    ? undefined
                    : SETTINGS_DESCRIPTIONS.permission_mode_word,
              }}
            >
              {(field) => (
                <Field
                  label={SETTINGS_DESCRIPTIONS.permission_mode}
                  hint={SETTINGS_DESCRIPTIONS.permission_mode_hint}
                  error={field.state.meta.errors.at(0)}
                >
                  <Combobox
                    value={field.state.value}
                    options={PERMISSION_MODES}
                    onValueChange={field.handleChange}
                    showOptionsLabel={SETTINGS_DESCRIPTIONS.show_permission_modes}
                  />
                </Field>
              )}
            </form.Field>
            <form.Field
              name="remote_control.capacity"
              validators={{
                onChange: ({ value }) =>
                  Number.isInteger(value) && value >= 1 && value <= 32
                    ? undefined
                    : SETTINGS_DESCRIPTIONS.capacity_range,
              }}
            >
              {(field) => (
                <Field
                  label={SETTINGS_DESCRIPTIONS.capacity}
                  hint={SETTINGS_DESCRIPTIONS.capacity_hint}
                  error={field.state.meta.errors.at(0)}
                >
                  <TextInput
                    type="number"
                    inputMode="numeric"
                    min={1}
                    max={32}
                    name={field.name}
                    value={Number.isNaN(field.state.value) ? "" : field.state.value}
                    onChange={(event) => field.handleChange(event.target.valueAsNumber)}
                    onBlur={field.handleBlur}
                  />
                </Field>
              )}
            </form.Field>
          </div>
        </div>
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
