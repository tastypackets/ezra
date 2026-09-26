import { getClaudeSettingsOptions } from "@ezra/client/react-query.gen";
import { useForm } from "@tanstack/react-form";
import { useQuery } from "@tanstack/react-query";
import { useId, useState } from "react";

import { Autocomplete } from "@/components/ui/autocomplete";
import type { AutocompleteOption } from "@/components/ui/autocomplete";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { CLONE_DESCRIPTIONS } from "@/content/folders";
import { useCloneActions } from "@/hooks/use-folder-actions";
import { defaultFolderName, folderNameProblem } from "@/lib/repositories";
import type { TakenNames } from "@/lib/repositories";
import { errorMessage } from "@/lib/utils";
import { gitHubRepositoriesQueryOptions } from "@/queries/git-queries";

/** "Clone repository" and the dialog it opens. */
export function CloneDialog({ taken }: { taken: TakenNames }) {
  const [open, setOpen] = useState(false);
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger render={<Button variant="outline" size="sm" />}>
        {CLONE_DESCRIPTIONS.open}
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{CLONE_DESCRIPTIONS.title}</DialogTitle>
        </DialogHeader>
        <CloneFormLoader taken={taken} onStarted={() => setOpen(false)} />
      </DialogContent>
    </Dialog>
  );
}

interface CloneFormProps {
  taken: TakenNames;
  onStarted: () => void;
}

/** Waits for the "Serve new repositories" setting, which the serve switch starts from. */
function CloneFormLoader({ taken, onStarted }: CloneFormProps) {
  const settings = useQuery(getClaudeSettingsOptions());
  if (settings.isPending) {
    return (
      <div className="flex justify-center py-6 text-muted-foreground">
        <Spinner />
      </div>
    );
  }
  return (
    <CloneForm
      taken={taken}
      onStarted={onStarted}
      serveByDefault={settings.data?.remote_control.serve_repositories ?? true}
    />
  );
}

function CloneForm({
  taken,
  onStarted,
  serveByDefault,
}: CloneFormProps & { serveByDefault: boolean }) {
  const ids = {
    repository: useId(),
    folder: useId(),
    folderHint: useId(),
    serve: useId(),
    serveLabel: useId(),
  };
  const { startClone } = useCloneActions();
  const repositories = useQuery(gitHubRepositoriesQueryOptions);
  const suggestions: AutocompleteOption[] = (repositories.data ?? []).map((repository) => ({
    value: repository.full_name,
    description: repository.description ?? undefined,
  }));
  const form = useForm({
    defaultValues: { repository: "", name: "", serve: serveByDefault },
    onSubmit: async ({ value }) => {
      try {
        await startClone.mutateAsync({
          body: {
            repository: value.repository.trim(),
            name: value.name.trim(),
            serve: value.serve,
          },
        });
      } catch {
        return;
      }
      onStarted();
    },
  });
  return (
    <form
      className="contents"
      onSubmit={(event) => {
        event.preventDefault();
        if (!form.state.isSubmitting) {
          void form.handleSubmit();
        }
      }}
    >
      <FieldGroup>
        <form.Field
          name="repository"
          validators={{
            onChange: ({ value }) =>
              value.trim() ? undefined : CLONE_DESCRIPTIONS.repository_required,
          }}
          listeners={{
            onChange: ({ value }) => {
              if (!form.getFieldMeta("name")?.isDirty) {
                form.setFieldValue("name", defaultFolderName(value), { dontUpdateMeta: true });
              }
            },
          }}
        >
          {(field) => {
            const error = field.state.meta.errors.at(0);
            return (
              <Field data-invalid={Boolean(error)}>
                <FieldLabel htmlFor={ids.repository}>{CLONE_DESCRIPTIONS.repository}</FieldLabel>
                <Autocomplete
                  id={ids.repository}
                  value={field.state.value}
                  options={suggestions}
                  onValueChange={field.handleChange}
                  showOptionsLabel={CLONE_DESCRIPTIONS.show_repositories}
                  aria-invalid={Boolean(error)}
                />
                <FieldDescription>{CLONE_DESCRIPTIONS.repository_hint}</FieldDescription>
                {error ? <FieldError>{error}</FieldError> : null}
              </Field>
            );
          }}
        </form.Field>
        <form.Field
          name="name"
          validators={{ onChange: ({ value }) => folderNameProblem(value, taken) }}
        >
          {(field) => {
            const error = field.state.meta.errors.at(0);
            return (
              <Field data-invalid={Boolean(error)}>
                <FieldLabel htmlFor={ids.folder}>{CLONE_DESCRIPTIONS.folder}</FieldLabel>
                <Input
                  id={ids.folder}
                  name={field.name}
                  value={field.state.value}
                  onChange={(event) => field.handleChange(event.target.value)}
                  onBlur={field.handleBlur}
                  spellCheck={false}
                  autoComplete="off"
                  aria-describedby={ids.folderHint}
                  aria-invalid={Boolean(error)}
                />
                <FieldDescription id={ids.folderHint}>
                  {CLONE_DESCRIPTIONS.folder_hint}
                </FieldDescription>
                {error ? <FieldError>{error}</FieldError> : null}
              </Field>
            );
          }}
        </form.Field>
        <form.Field name="serve">
          {(field) => (
            <Field orientation="horizontal">
              <Switch
                id={ids.serve}
                aria-labelledby={ids.serveLabel}
                checked={field.state.value}
                onCheckedChange={field.handleChange}
              />
              <FieldLabel id={ids.serveLabel} htmlFor={ids.serve}>
                {CLONE_DESCRIPTIONS.serve}
              </FieldLabel>
            </Field>
          )}
        </form.Field>
      </FieldGroup>
      {startClone.isError ? (
        <p role="alert" className="text-destructive">
          {errorMessage(startClone.error)}
        </p>
      ) : null}
      <DialogFooter>
        <DialogClose render={<Button variant="outline" />}>{CLONE_DESCRIPTIONS.cancel}</DialogClose>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <Button type="submit" loading={isSubmitting}>
              {CLONE_DESCRIPTIONS.submit}
            </Button>
          )}
        </form.Subscribe>
      </DialogFooter>
    </form>
  );
}
