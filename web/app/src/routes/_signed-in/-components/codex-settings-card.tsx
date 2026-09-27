import { getCodexSettingsOptions, updateCodexSettingsMutation } from "@ezra/client/react-query.gen";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useId } from "react";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { RadioGroup } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/toast";
import { AGENT_NAMES } from "@/content/agents";
import {
  APPROVAL_POLICIES,
  APPROVAL_POLICY_ORDER,
  SANDBOX_MODE_ORDER,
  SANDBOX_MODES,
  SETTINGS_DESCRIPTIONS,
} from "@/content/settings";
import { SETTINGS_FILE_DESCRIPTIONS } from "@/content/settings-file";
import { errorMessage } from "@/lib/utils";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

import { RadioChoice } from "./radio-choice";
import { SettingsCardFooter } from "./settings-card-footer";
import { SettingsFileSection, useSettingsFileDraft } from "./settings-file-section";

/** How the manager serves this box to the ChatGPT app through Codex, and Codex's own config.toml. */
export function CodexSettingsCard() {
  const queryClient = useQueryClient();
  const ids = {
    title: useId(),
    enabled: useId(),
    enabledLabel: useId(),
    enabledHint: useId(),
    sandbox: useId(),
    sandboxHint: useId(),
    approvals: useId(),
    approvalsHint: useId(),
  };
  const { data: settings } = useSuspenseQuery(getCodexSettingsOptions());
  const { data: pickerBlocked } = useSuspenseQuery({
    ...remoteControlQueryOptions,
    select: (overview) => overview.codex.folder_picker === "blocked",
  });
  const save = useMutation({
    ...updateCodexSettingsMutation(),
    onSuccess: (saved) => {
      queryClient.setQueryData(getCodexSettingsOptions().queryKey, saved);
    },
  });
  const file = useSettingsFileDraft("codex", true);
  const form = useForm({
    defaultValues: settings,
    onSubmit: async ({ value, formApi }) => {
      const fileChanged = file.dirty;
      const settingsChanged = !formApi.state.isDefaultValue || !fileChanged;
      try {
        if (settingsChanged) {
          formApi.reset(await save.mutateAsync({ body: value }));
        }
        if (fileChanged) {
          await file.save();
        }
      } catch {
        return;
      }
      toast.add({
        title:
          settingsChanged || !file.file
            ? SETTINGS_DESCRIPTIONS.codex_saved
            : SETTINGS_FILE_DESCRIPTIONS.saved(file.file.path),
      });
    },
  });
  return (
    <Card id="codex" aria-labelledby={ids.title}>
      <CardHeader>
        <CardTitle id={ids.title}>{AGENT_NAMES.codex}</CardTitle>
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
          <FieldSet>
            <FieldLegend>{SETTINGS_DESCRIPTIONS.codex_remote_control}</FieldLegend>
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
                      {SETTINGS_DESCRIPTIONS.codex_remote_enabled}
                    </FieldLabel>
                    <FieldDescription id={ids.enabledHint}>
                      {SETTINGS_DESCRIPTIONS.codex_remote_enabled_hint}
                    </FieldDescription>
                  </FieldContent>
                </Field>
              )}
            </form.Field>
            <form.Field name="remote_control.sandbox">
              {(field) => (
                <FieldSet>
                  <FieldLegend id={ids.sandbox} variant="label">
                    {SETTINGS_DESCRIPTIONS.codex_sandbox}
                  </FieldLegend>
                  <FieldDescription id={ids.sandboxHint}>
                    {pickerBlocked
                      ? SETTINGS_DESCRIPTIONS.codex_sandbox_blocked_hint
                      : SETTINGS_DESCRIPTIONS.codex_sandbox_hint}
                  </FieldDescription>
                  <RadioGroup
                    aria-labelledby={ids.sandbox}
                    aria-describedby={ids.sandboxHint}
                    value={field.state.value}
                    onValueChange={(next) => {
                      const sandbox = SANDBOX_MODE_ORDER.find((candidate) => candidate === next);
                      if (sandbox) {
                        field.handleChange(sandbox);
                      }
                    }}
                    className="grid gap-2 md:grid-cols-3"
                  >
                    {SANDBOX_MODE_ORDER.map((mode) => (
                      <RadioChoice key={mode} value={mode} {...SANDBOX_MODES[mode]} />
                    ))}
                  </RadioGroup>
                </FieldSet>
              )}
            </form.Field>
            <form.Field name="remote_control.approvals">
              {(field) => (
                <FieldSet>
                  <FieldLegend id={ids.approvals} variant="label">
                    {SETTINGS_DESCRIPTIONS.codex_approvals}
                  </FieldLegend>
                  <FieldDescription id={ids.approvalsHint}>
                    {SETTINGS_DESCRIPTIONS.codex_approvals_hint}
                  </FieldDescription>
                  <RadioGroup
                    aria-labelledby={ids.approvals}
                    aria-describedby={ids.approvalsHint}
                    value={field.state.value}
                    onValueChange={(next) => {
                      const approvals = APPROVAL_POLICY_ORDER.find(
                        (candidate) => candidate === next,
                      );
                      if (approvals) {
                        field.handleChange(approvals);
                      }
                    }}
                    className="grid gap-2 sm:grid-cols-2"
                  >
                    {APPROVAL_POLICY_ORDER.map((policy) => (
                      <RadioChoice key={policy} value={policy} {...APPROVAL_POLICIES[policy]} />
                    ))}
                  </RadioGroup>
                </FieldSet>
              )}
            </form.Field>
          </FieldSet>
        </CardContent>
        <SettingsFileSection draft={file} />
        <form.Subscribe selector={(state) => [state.isSubmitting, !state.isDefaultValue] as const}>
          {([isSubmitting, settingsChanged]) => (
            <SettingsCardFooter
              error={save.isError ? errorMessage(save.error) : undefined}
              file={file}
              submitting={isSubmitting}
              settingsChanged={settingsChanged}
              onRevert={() => {
                form.reset();
                save.reset();
                file.revert();
              }}
            />
          )}
        </form.Subscribe>
      </form>
    </Card>
  );
}
