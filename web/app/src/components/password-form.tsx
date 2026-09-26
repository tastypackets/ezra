import { useForm } from "@tanstack/react-form";
import type { UseMutationResult } from "@tanstack/react-query";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Field, TextInput } from "@/components/ui/field";
import { SESSION_DESCRIPTIONS } from "@/content/session";
import { errorMessage } from "@/lib/utils";

export interface PasswordFormProps {
  title: string;
  description: string;
  submitLabel: string;
  autoComplete: "new-password" | "current-password";
  mutation: UseMutationResult<unknown, unknown, string>;
  /** Runs after the password is accepted. The form stays busy until it resolves. */
  onSuccess: () => Promise<void>;
}

/** A small centered card that submits one password. */
export function PasswordForm({
  title,
  description,
  submitLabel,
  autoComplete,
  mutation,
  onSuccess,
}: PasswordFormProps) {
  const form = useForm({
    defaultValues: { password: "" },
    onSubmit: async ({ value }) => {
      try {
        await mutation.mutateAsync(value.password);
      } catch {
        return;
      }
      await onSuccess();
    },
  });
  return (
    <Card className="mx-auto mt-16 max-w-sm p-5">
      <h2 className="text-base font-semibold">{title}</h2>
      <p className="mt-0.5 text-ez-muted">{description}</p>
      <form
        className="mt-4 flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          if (!form.state.isSubmitting) {
            void form.handleSubmit();
          }
        }}
      >
        <form.Field name="password">
          {(field) => (
            <Field
              label={SESSION_DESCRIPTIONS.password_label}
              error={mutation.isError ? errorMessage(mutation.error) : undefined}
            >
              <TextInput
                type="password"
                name={field.name}
                value={field.state.value}
                onChange={(event) => field.handleChange(event.target.value)}
                onBlur={field.handleBlur}
                autoComplete={autoComplete}
                required
                autoFocus
              />
            </Field>
          )}
        </form.Field>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <Button type="submit" variant="primary" loading={isSubmitting}>
              {submitLabel}
            </Button>
          )}
        </form.Subscribe>
      </form>
    </Card>
  );
}
