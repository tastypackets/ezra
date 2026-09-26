import { useForm } from "@tanstack/react-form";
import type { UseMutationResult } from "@tanstack/react-query";
import { useId } from "react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
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
  const passwordId = useId();
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
  const failure = mutation.isError ? errorMessage(mutation.error) : undefined;
  return (
    <Card className="mx-auto mt-16 max-w-sm">
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <CardDescription>{description}</CardDescription>
      </CardHeader>
      <CardContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (!form.state.isSubmitting) {
              void form.handleSubmit();
            }
          }}
        >
          <form.Field name="password">
            {(field) => (
              <Field data-invalid={Boolean(failure)}>
                <FieldLabel htmlFor={passwordId}>{SESSION_DESCRIPTIONS.password_label}</FieldLabel>
                <Input
                  id={passwordId}
                  type="password"
                  name={field.name}
                  value={field.state.value}
                  onChange={(event) => field.handleChange(event.target.value)}
                  onBlur={field.handleBlur}
                  autoComplete={autoComplete}
                  aria-invalid={Boolean(failure)}
                  required
                  autoFocus
                />
                {failure ? <FieldError>{failure}</FieldError> : null}
              </Field>
            )}
          </form.Field>
          <form.Subscribe selector={(state) => state.isSubmitting}>
            {(isSubmitting) => (
              <Button type="submit" loading={isSubmitting}>
                {submitLabel}
              </Button>
            )}
          </form.Subscribe>
        </form>
      </CardContent>
    </Card>
  );
}
