import type { FolderStatus, SpawnMode } from "@ezra/client";
import { getClaudeSettingsOptions } from "@ezra/client/react-query.gen";
import { useForm } from "@tanstack/react-form";
import { useQuery } from "@tanstack/react-query";
import { useId } from "react";

import { Autocomplete } from "@/components/ui/autocomplete";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { RadioGroup } from "@/components/ui/radio-group";
import { CLAUDE_OPTIONS_DESCRIPTIONS, SPAWN_MODE_ORDER, SPAWN_MODES } from "@/content/folders";
import { PERMISSION_MODE_NAMES, PERMISSION_MODES, SETTINGS_DESCRIPTIONS } from "@/content/settings";
import { useFolderActions } from "@/hooks/use-folder-actions";
import { errorMessage } from "@/lib/utils";

import { RadioChoice } from "./radio-choice";

const FOLLOW_SETTINGS = "settings" as const;
const SPAWN_CHOICES: readonly (SpawnMode | typeof FOLLOW_SETTINGS)[] = [
  FOLLOW_SETTINGS,
  ...SPAWN_MODE_ORDER,
];

export interface ClaudeOptionsDialogProps {
  folder: FolderStatus;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** A folder's own Claude Code options, each empty one following Settings. */
export function ClaudeOptionsDialog({ folder, open, onOpenChange }: ClaudeOptionsDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{CLAUDE_OPTIONS_DESCRIPTIONS.title(folder.name)}</DialogTitle>
          <DialogDescription>{CLAUDE_OPTIONS_DESCRIPTIONS.description}</DialogDescription>
        </DialogHeader>
        {open ? <ClaudeOptionsForm folder={folder} onSaved={() => onOpenChange(false)} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function ClaudeOptionsForm({ folder, onSaved }: { folder: FolderStatus; onSaved: () => void }) {
  const settings = useQuery(getClaudeSettingsOptions());
  const { chooseClaudeOptions } = useFolderActions();
  const ids = {
    spawn: useId(),
    permissionMode: useId(),
    permissionModeError: useId(),
    capacity: useId(),
    capacityError: useId(),
  };
  const form = useForm({
    defaultValues: {
      spawn: folder.claude.spawn ?? FOLLOW_SETTINGS,
      permission_mode: folder.claude.permission_mode ?? "",
      capacity: folder.claude.capacity ?? null,
    },
    onSubmit: async ({ value }) => {
      try {
        await chooseClaudeOptions.mutateAsync({
          path: { name: folder.name },
          body: {
            spawn: value.spawn === FOLLOW_SETTINGS ? undefined : value.spawn,
            permission_mode: value.permission_mode.trim() || undefined,
            capacity: value.capacity ?? undefined,
          },
        });
      } catch {
        return;
      }
      onSaved();
    },
  });
  const defaults = settings.data?.remote_control;
  return (
    <form
      className="flex flex-col gap-6"
      onSubmit={(event) => {
        event.preventDefault();
        if (!form.state.isSubmitting) {
          void form.handleSubmit();
        }
      }}
    >
      <FieldGroup>
        {folder.git ? (
          <form.Field name="spawn">
            {(field) => (
              <FieldSet>
                <FieldLegend id={ids.spawn} variant="label">
                  {CLAUDE_OPTIONS_DESCRIPTIONS.spawn}
                </FieldLegend>
                <RadioGroup
                  aria-labelledby={ids.spawn}
                  value={field.state.value}
                  onValueChange={(next) => {
                    const spawn = SPAWN_CHOICES.find((candidate) => candidate === next);
                    if (spawn) {
                      field.handleChange(spawn);
                    }
                  }}
                >
                  <RadioChoice
                    value={FOLLOW_SETTINGS}
                    title={CLAUDE_OPTIONS_DESCRIPTIONS.spawn_default}
                    description={defaults ? SPAWN_MODES[defaults.spawn].title : ""}
                  />
                  {SPAWN_MODE_ORDER.map((mode) => (
                    <RadioChoice key={mode} value={mode} {...SPAWN_MODES[mode]} />
                  ))}
                </RadioGroup>
              </FieldSet>
            )}
          </form.Field>
        ) : (
          <FieldDescription>{CLAUDE_OPTIONS_DESCRIPTIONS.spawn_in_folder}</FieldDescription>
        )}
        <div className="grid gap-4 sm:grid-cols-2">
          <form.Field
            name="permission_mode"
            validators={{
              onChange: ({ value }) =>
                value.trim() === "" || PERMISSION_MODE_NAMES.has(value.trim())
                  ? undefined
                  : CLAUDE_OPTIONS_DESCRIPTIONS.permission_mode_unknown,
            }}
          >
            {(field) => {
              const error = field.state.meta.errors.at(0);
              return (
                <Field data-invalid={Boolean(error)}>
                  <FieldLabel htmlFor={ids.permissionMode}>
                    {CLAUDE_OPTIONS_DESCRIPTIONS.permission_mode}
                  </FieldLabel>
                  <Autocomplete
                    id={ids.permissionMode}
                    value={field.state.value}
                    options={PERMISSION_MODES}
                    onValueChange={field.handleChange}
                    showOptionsLabel={SETTINGS_DESCRIPTIONS.show_permission_modes}
                    placeholder={
                      defaults
                        ? CLAUDE_OPTIONS_DESCRIPTIONS.permission_mode_default(
                            defaults.permission_mode,
                          )
                        : undefined
                    }
                    aria-invalid={Boolean(error)}
                    aria-describedby={error ? ids.permissionModeError : undefined}
                  />
                  {error ? <FieldError id={ids.permissionModeError}>{error}</FieldError> : null}
                </Field>
              );
            }}
          </form.Field>
          <form.Field
            name="capacity"
            validators={{
              onChange: ({ value }) =>
                value == null || (Number.isInteger(value) && value >= 1)
                  ? undefined
                  : CLAUDE_OPTIONS_DESCRIPTIONS.capacity_range,
            }}
          >
            {(field) => {
              const error = field.state.meta.errors.at(0);
              return (
                <Field data-invalid={Boolean(error)}>
                  <FieldLabel htmlFor={ids.capacity}>
                    {CLAUDE_OPTIONS_DESCRIPTIONS.capacity}
                  </FieldLabel>
                  <Input
                    id={ids.capacity}
                    type="number"
                    inputMode="numeric"
                    min={1}
                    name={field.name}
                    placeholder={
                      defaults
                        ? CLAUDE_OPTIONS_DESCRIPTIONS.capacity_default(defaults.capacity)
                        : undefined
                    }
                    value={field.state.value ?? ""}
                    onChange={(event) =>
                      field.handleChange(
                        event.target.value === "" ? null : event.target.valueAsNumber,
                      )
                    }
                    onBlur={field.handleBlur}
                    aria-invalid={Boolean(error)}
                    aria-describedby={error ? ids.capacityError : undefined}
                  />
                  {error ? <FieldError id={ids.capacityError}>{error}</FieldError> : null}
                </Field>
              );
            }}
          </form.Field>
        </div>
      </FieldGroup>
      {chooseClaudeOptions.isError ? (
        <p role="alert" className="text-destructive">
          {errorMessage(chooseClaudeOptions.error)}
        </p>
      ) : null}
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>
          {CLAUDE_OPTIONS_DESCRIPTIONS.cancel}
        </DialogClose>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <Button type="submit" loading={isSubmitting}>
              {CLAUDE_OPTIONS_DESCRIPTIONS.save}
            </Button>
          )}
        </form.Subscribe>
      </DialogFooter>
    </form>
  );
}
