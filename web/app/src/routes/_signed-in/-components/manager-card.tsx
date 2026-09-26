import type { CertificateStatus } from "@ezra/client";
import { useForm } from "@tanstack/react-form";
import { useSuspenseQuery } from "@tanstack/react-query";
import { useId } from "react";

import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { MANAGER_DESCRIPTIONS } from "@/content/manager";
import { useManagerActions } from "@/hooks/use-manager-actions";
import { errorMessage } from "@/lib/utils";
import { managerQueryOptions } from "@/queries/manager-queries";

/** The manager's password and HTTPS certificate. */
export function ManagerCard() {
  const { data: manager } = useSuspenseQuery(managerQueryOptions);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{MANAGER_DESCRIPTIONS.title}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-6">
        <PasswordForm />
        <Separator />
        <CertificateSection certificate={manager.certificate} />
      </CardContent>
    </Card>
  );
}

function PasswordForm() {
  const { changePassword } = useManagerActions();
  const ids = { current: useId(), currentError: useId(), new: useId() };
  const form = useForm({
    defaultValues: { current_password: "", new_password: "" },
    onSubmit: async ({ value, formApi }) => {
      try {
        await changePassword.mutateAsync({ body: value });
      } catch {
        return;
      }
      formApi.reset();
    },
  });
  const failure = changePassword.isError ? errorMessage(changePassword.error) : undefined;
  return (
    <section className="flex flex-col gap-3">
      <h3 className="font-medium">{MANAGER_DESCRIPTIONS.password}</h3>
      <form
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          if (!form.state.isSubmitting) {
            void form.handleSubmit();
          }
        }}
      >
        <FieldGroup className="grid gap-4 sm:grid-cols-2">
          <form.Field name="current_password">
            {(field) => (
              <Field data-invalid={Boolean(failure)}>
                <FieldLabel htmlFor={ids.current}>
                  {MANAGER_DESCRIPTIONS.current_password}
                </FieldLabel>
                <Input
                  id={ids.current}
                  type="password"
                  name={field.name}
                  value={field.state.value}
                  onChange={(event) => field.handleChange(event.target.value)}
                  onBlur={field.handleBlur}
                  autoComplete="current-password"
                  aria-invalid={Boolean(failure)}
                  aria-describedby={failure ? ids.currentError : undefined}
                  required
                />
                {failure ? <FieldError id={ids.currentError}>{failure}</FieldError> : null}
              </Field>
            )}
          </form.Field>
          <form.Field name="new_password">
            {(field) => (
              <Field>
                <FieldLabel htmlFor={ids.new}>{MANAGER_DESCRIPTIONS.new_password}</FieldLabel>
                <Input
                  id={ids.new}
                  type="password"
                  name={field.name}
                  value={field.state.value}
                  onChange={(event) => field.handleChange(event.target.value)}
                  onBlur={field.handleBlur}
                  autoComplete="new-password"
                  required
                />
              </Field>
            )}
          </form.Field>
        </FieldGroup>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <Button type="submit" className="self-start" loading={isSubmitting}>
              {MANAGER_DESCRIPTIONS.change_password}
            </Button>
          )}
        </form.Subscribe>
      </form>
    </section>
  );
}

function CertificateSection({ certificate }: { certificate?: CertificateStatus | null }) {
  const { regenerateCertificate } = useManagerActions();
  return (
    <section className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <h3 className="font-medium">{MANAGER_DESCRIPTIONS.certificate}</h3>
          <Badge variant="secondary">{MANAGER_DESCRIPTIONS.self_signed}</Badge>
        </div>
        {certificate ? (
          <AlertDialog>
            <AlertDialogTrigger
              render={<Button variant="outline" loading={regenerateCertificate.isPending} />}
            >
              {MANAGER_DESCRIPTIONS.regenerate}
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>{MANAGER_DESCRIPTIONS.regenerate_title}</AlertDialogTitle>
                <AlertDialogDescription>
                  {MANAGER_DESCRIPTIONS.regenerate_description}
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>{MANAGER_DESCRIPTIONS.cancel}</AlertDialogCancel>
                <AlertDialogCancel
                  variant="default"
                  onClick={() => regenerateCertificate.mutate({})}
                >
                  {MANAGER_DESCRIPTIONS.regenerate_confirm}
                </AlertDialogCancel>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        ) : null}
      </div>
      {certificate ? (
        <p className="text-muted-foreground">
          {MANAGER_DESCRIPTIONS.expires(
            new Date(certificate.expires_at * 1000).toLocaleDateString(undefined, {
              dateStyle: "medium",
            }),
          )}
        </p>
      ) : (
        <p className="text-muted-foreground">{MANAGER_DESCRIPTIONS.no_certificate}</p>
      )}
      {regenerateCertificate.isError ? (
        <p role="alert" className="text-destructive">
          {errorMessage(regenerateCertificate.error)}
        </p>
      ) : null}
    </section>
  );
}
