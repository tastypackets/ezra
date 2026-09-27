import type { ReleaseChannel } from "@ezra/client";
import {
  getClaudeSettingsOptions,
  updateClaudeSettingsMutation,
} from "@ezra/client/react-query.gen";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useId } from "react";

import { Autocomplete } from "@/components/ui/autocomplete";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { RadioGroup } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { toastManager } from "@/components/ui/toast";
import { AGENT_NAMES } from "@/content/agents";
import { SPAWN_MODE_ORDER, SPAWN_MODES } from "@/content/folders";
import {
  PERMISSION_MODE_NAMES,
  PERMISSION_MODES,
  RELEASE_CHANNELS,
  SETTINGS_DESCRIPTIONS,
} from "@/content/settings";
import { errorMessage } from "@/lib/utils";
import { agentsQueryOptions, isClaudeInstalled } from "@/queries/agent-queries";
import { foldersQueryOptions } from "@/queries/folder-queries";

import { RadioChoice } from "./radio-choice";

const CHANNEL_ORDER: readonly ReleaseChannel[] = ["latest", "stable"];

/** How the manager installs, updates and serves Claude Code, with only the release channel until it is installed. */
export function ClaudeSettingsCard() {
  const queryClient = useQueryClient();
  const ids = {
    enabled: useId(),
    enabledLabel: useId(),
    enabledHint: useId(),
    serveRepositories: useId(),
    serveRepositoriesLabel: useId(),
    serveRepositoriesHint: useId(),
    permissionMode: useId(),
    permissionModeHint: useId(),
    permissionModeError: useId(),
    capacity: useId(),
    capacityHint: useId(),
    capacityError: useId(),
    spawn: useId(),
    spawnHint: useId(),
    releaseChannel: useId(),
    releaseChannelHint: useId(),
  };
  const { data: settings } = useSuspenseQuery(getClaudeSettingsOptions());
  const { data: installed } = useSuspenseQuery({
    ...agentsQueryOptions,
    select: isClaudeInstalled,
  });
  const save = useMutation({
    ...updateClaudeSettingsMutation(),
    onSuccess: (saved) => {
      queryClient.setQueryData(getClaudeSettingsOptions().queryKey, saved);
      void queryClient.invalidateQueries({ queryKey: agentsQueryOptions.queryKey });
      void queryClient.invalidateQueries({ queryKey: foldersQueryOptions.queryKey });
      toastManager.add({ title: SETTINGS_DESCRIPTIONS.saved });
    },
  });
  const form = useForm({
    defaultValues: settings,
    onSubmit: async ({ value, formApi }) => {
      try {
        formApi.reset(await save.mutateAsync({ body: value }));
      } catch {
        return;
      }
    },
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{AGENT_NAMES.claude}</CardTitle>
      </CardHeader>
      <form
        className="contents"
        onSubmit={(event) => {
          event.preventDefault();
          if (!form.state.isSubmitting) {
            void form.handleSubmit();
          }
        }}
      >
        <CardContent>
          <FieldGroup>
            {installed ? (
              <FieldSet>
                <FieldLegend>{SETTINGS_DESCRIPTIONS.remote_control}</FieldLegend>
                <form.Field name="remote_control.enabled">
                  {(field) => (
                    <Field orientation="horizontal">
                      <Switch
                        id={ids.enabled}
                        aria-labelledby={ids.enabledLabel}
                        aria-describedby={ids.enabledHint}
                        checked={field.state.value}
                        onCheckedChange={field.handleChange}
                      />
                      <FieldContent>
                        <FieldLabel id={ids.enabledLabel} htmlFor={ids.enabled}>
                          {SETTINGS_DESCRIPTIONS.remote_control_enabled}
                        </FieldLabel>
                        <FieldDescription id={ids.enabledHint}>
                          {SETTINGS_DESCRIPTIONS.remote_control_enabled_hint}
                        </FieldDescription>
                      </FieldContent>
                    </Field>
                  )}
                </form.Field>
                <form.Field name="remote_control.serve_repositories">
                  {(field) => (
                    <Field orientation="horizontal">
                      <Switch
                        id={ids.serveRepositories}
                        aria-labelledby={ids.serveRepositoriesLabel}
                        aria-describedby={ids.serveRepositoriesHint}
                        checked={field.state.value}
                        onCheckedChange={field.handleChange}
                      />
                      <FieldContent>
                        <FieldLabel id={ids.serveRepositoriesLabel} htmlFor={ids.serveRepositories}>
                          {SETTINGS_DESCRIPTIONS.serve_repositories}
                        </FieldLabel>
                        <FieldDescription id={ids.serveRepositoriesHint}>
                          {SETTINGS_DESCRIPTIONS.serve_repositories_hint}
                        </FieldDescription>
                      </FieldContent>
                    </Field>
                  )}
                </form.Field>
                <form.Field name="remote_control.spawn">
                  {(field) => (
                    <FieldSet>
                      <FieldLegend id={ids.spawn} variant="label">
                        {SETTINGS_DESCRIPTIONS.spawn}
                      </FieldLegend>
                      <FieldDescription id={ids.spawnHint}>
                        {SETTINGS_DESCRIPTIONS.spawn_hint}
                      </FieldDescription>
                      <RadioGroup
                        aria-labelledby={ids.spawn}
                        aria-describedby={ids.spawnHint}
                        value={field.state.value}
                        onValueChange={(next) => {
                          const spawn = SPAWN_MODE_ORDER.find((candidate) => candidate === next);
                          if (spawn) {
                            field.handleChange(spawn);
                          }
                        }}
                        className="grid gap-2 sm:grid-cols-2"
                      >
                        {SPAWN_MODE_ORDER.map((mode) => (
                          <RadioChoice key={mode} value={mode} {...SPAWN_MODES[mode]} />
                        ))}
                      </RadioGroup>
                    </FieldSet>
                  )}
                </form.Field>
                <div className="grid gap-4 sm:grid-cols-2">
                  <form.Field
                    name="remote_control.permission_mode"
                    validators={{
                      onChange: ({ value }) =>
                        PERMISSION_MODE_NAMES.has(value.trim())
                          ? undefined
                          : SETTINGS_DESCRIPTIONS.permission_mode_unknown,
                    }}
                  >
                    {(field) => {
                      const error = field.state.meta.errors.at(0);
                      return (
                        <Field data-invalid={Boolean(error)}>
                          <FieldLabel htmlFor={ids.permissionMode}>
                            {SETTINGS_DESCRIPTIONS.permission_mode}
                          </FieldLabel>
                          <Autocomplete
                            id={ids.permissionMode}
                            value={field.state.value}
                            options={PERMISSION_MODES}
                            onValueChange={field.handleChange}
                            showOptionsLabel={SETTINGS_DESCRIPTIONS.show_permission_modes}
                            aria-invalid={Boolean(error)}
                            aria-describedby={
                              error
                                ? `${ids.permissionModeHint} ${ids.permissionModeError}`
                                : ids.permissionModeHint
                            }
                          />
                          <FieldDescription id={ids.permissionModeHint}>
                            {SETTINGS_DESCRIPTIONS.permission_mode_hint}
                          </FieldDescription>
                          {error ? (
                            <FieldError id={ids.permissionModeError}>{error}</FieldError>
                          ) : null}
                        </Field>
                      );
                    }}
                  </form.Field>
                  <form.Field
                    name="remote_control.capacity"
                    validators={{
                      onChange: ({ value }) =>
                        value == null || (Number.isInteger(value) && value >= 1)
                          ? undefined
                          : SETTINGS_DESCRIPTIONS.capacity_range,
                    }}
                  >
                    {(field) => {
                      const error = field.state.meta.errors.at(0);
                      return (
                        <Field data-invalid={Boolean(error)}>
                          <FieldLabel htmlFor={ids.capacity}>
                            {SETTINGS_DESCRIPTIONS.capacity}
                          </FieldLabel>
                          <Input
                            id={ids.capacity}
                            type="number"
                            inputMode="numeric"
                            min={1}
                            name={field.name}
                            placeholder={SETTINGS_DESCRIPTIONS.capacity_default}
                            value={field.state.value ?? ""}
                            onChange={(event) =>
                              field.handleChange(
                                event.target.value === "" ? null : event.target.valueAsNumber,
                              )
                            }
                            onBlur={field.handleBlur}
                            aria-invalid={Boolean(error)}
                            aria-describedby={
                              error ? `${ids.capacityHint} ${ids.capacityError}` : ids.capacityHint
                            }
                          />
                          <FieldDescription id={ids.capacityHint}>
                            {SETTINGS_DESCRIPTIONS.capacity_hint}
                          </FieldDescription>
                          {error ? <FieldError id={ids.capacityError}>{error}</FieldError> : null}
                        </Field>
                      );
                    }}
                  </form.Field>
                </div>
              </FieldSet>
            ) : null}
            <form.Field name="release_channel">
              {(field) => (
                <FieldSet>
                  <FieldLegend id={ids.releaseChannel}>
                    {SETTINGS_DESCRIPTIONS.release_channel}
                  </FieldLegend>
                  <FieldDescription id={ids.releaseChannelHint}>
                    {SETTINGS_DESCRIPTIONS.release_channel_hint}
                  </FieldDescription>
                  <RadioGroup
                    aria-labelledby={ids.releaseChannel}
                    aria-describedby={ids.releaseChannelHint}
                    value={field.state.value}
                    onValueChange={(next) => {
                      const channel = CHANNEL_ORDER.find((candidate) => candidate === next);
                      if (channel) {
                        field.handleChange(channel);
                      }
                    }}
                    className="grid gap-2 sm:grid-cols-2"
                  >
                    {CHANNEL_ORDER.map((channel) => (
                      <RadioChoice key={channel} value={channel} {...RELEASE_CHANNELS[channel]} />
                    ))}
                  </RadioGroup>
                </FieldSet>
              )}
            </form.Field>
          </FieldGroup>
        </CardContent>
        <CardFooter className="justify-between gap-4">
          <p role="alert" className="text-destructive">
            {save.isError ? errorMessage(save.error) : null}
          </p>
          <form.Subscribe selector={(state) => state.isSubmitting}>
            {(isSubmitting) => (
              <Button type="submit" loading={isSubmitting}>
                {SETTINGS_DESCRIPTIONS.save}
              </Button>
            )}
          </form.Subscribe>
        </CardFooter>
      </form>
    </Card>
  );
}
