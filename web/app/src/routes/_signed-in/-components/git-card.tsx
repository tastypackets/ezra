import type { CommitIdentity, GitHubStatus } from "@ezra/client";
import { useForm } from "@tanstack/react-form";
import { useSuspenseQuery } from "@tanstack/react-query";
import { useId } from "react";

import { SignInSteps, WaitingForWebsite } from "@/components/sign-in-steps";
import { Badge } from "@/components/ui/badge";
import type { badgeVariants } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { GIT_DESCRIPTIONS } from "@/content/git";
import { useGitActions } from "@/hooks/use-git-actions";
import { errorMessage } from "@/lib/utils";
import { gitStatusQueryOptions } from "@/queries/git-queries";

type BadgeVariant = NonNullable<Parameters<typeof badgeVariants>[0]>["variant"];

/** The GitHub sign-in and commit identity every agent's git uses. */
export function GitCard() {
  const { data: git } = useSuspenseQuery(gitStatusQueryOptions);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{GIT_DESCRIPTIONS.title}</CardTitle>
        <CardDescription>{GIT_DESCRIPTIONS.description}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-6">
        <GitHubSection github={git.github} />
        <Separator />
        <IdentityForm identity={git.identity} />
      </CardContent>
    </Card>
  );
}

function GitHubSection({ github }: { github: GitHubStatus }) {
  const { startGitHubSignIn, signOutOfGitHub } = useGitActions();
  const failed = [startGitHubSignIn, signOutOfGitHub]
    .filter((mutation) => mutation.isError)
    .toSorted((first, second) => second.submittedAt - first.submittedAt)
    .at(0);
  const state = gitHubState(github);
  const canSignOut = !github.from_environment && (github.signed_in || github.failing);
  const canSignIn =
    !github.from_environment && !github.signed_in && !github.failing && !github.login_prompt;
  return (
    <section className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="font-medium">{GIT_DESCRIPTIONS.github}</h3>
          <Badge variant={state.variant}>{state.label}</Badge>
        </div>
        {canSignOut ? (
          <Button
            variant="outline"
            size="sm"
            loading={signOutOfGitHub.isPending}
            onClick={() => signOutOfGitHub.mutate()}
          >
            {GIT_DESCRIPTIONS.sign_out}
          </Button>
        ) : null}
        {canSignIn ? (
          <Button
            size="sm"
            loading={startGitHubSignIn.isPending}
            onClick={() => startGitHubSignIn.mutate()}
          >
            {GIT_DESCRIPTIONS.sign_in}
          </Button>
        ) : null}
      </div>
      {github.from_environment ? (
        <p className="text-muted-foreground">{GIT_DESCRIPTIONS.from_environment}</p>
      ) : null}
      {github.failing ? <p className="text-destructive">{GIT_DESCRIPTIONS.failing}</p> : null}
      {failed ? (
        <p role="alert" className="text-destructive">
          {errorMessage(failed.error)}
        </p>
      ) : null}
      {github.login_prompt ? (
        <div className="flex flex-col gap-4 rounded-lg border p-4">
          <SignInSteps prompt={github.login_prompt} />
          <div className="flex flex-wrap items-center justify-between gap-3">
            <WaitingForWebsite />
            <Button
              variant="outline"
              size="sm"
              loading={startGitHubSignIn.isPending}
              onClick={() => startGitHubSignIn.mutate()}
            >
              {GIT_DESCRIPTIONS.start_over}
            </Button>
          </div>
        </div>
      ) : null}
    </section>
  );
}

function gitHubState(github: GitHubStatus): { label: string; variant: BadgeVariant } {
  if (github.login_prompt) {
    return { label: GIT_DESCRIPTIONS.signing_in, variant: "warning" };
  }
  if (github.signed_in) {
    return {
      label: github.account
        ? GIT_DESCRIPTIONS.signed_in_as(github.account)
        : GIT_DESCRIPTIONS.signed_in,
      variant: "success",
    };
  }
  if (github.failing) {
    return { label: GIT_DESCRIPTIONS.not_confirmed, variant: "warning" };
  }
  return { label: GIT_DESCRIPTIONS.signed_out, variant: "secondary" };
}

function IdentityForm({ identity }: { identity: CommitIdentity }) {
  const { saveIdentity } = useGitActions();
  const nameId = useId();
  const emailId = useId();
  const form = useForm({
    defaultValues: { name: identity.name ?? "", email: identity.email ?? "" },
    onSubmit: async ({ value, formApi }) => {
      try {
        const saved = await saveIdentity.mutateAsync(value);
        formApi.reset({ name: saved.name ?? "", email: saved.email ?? "" });
      } catch {
        return;
      }
    },
  });
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (!form.state.isSubmitting) {
          void form.handleSubmit();
        }
      }}
    >
      <FieldSet>
        <FieldLegend>{GIT_DESCRIPTIONS.identity}</FieldLegend>
        <FieldGroup className="grid gap-4 sm:grid-cols-2">
          <form.Field name="name">
            {(field) => (
              <Field>
                <FieldLabel htmlFor={nameId}>{GIT_DESCRIPTIONS.name}</FieldLabel>
                <Input
                  id={nameId}
                  name={field.name}
                  value={field.state.value}
                  onChange={(event) => field.handleChange(event.target.value)}
                  onBlur={field.handleBlur}
                  autoComplete="name"
                />
              </Field>
            )}
          </form.Field>
          <form.Field name="email">
            {(field) => (
              <Field>
                <FieldLabel htmlFor={emailId}>{GIT_DESCRIPTIONS.email}</FieldLabel>
                <Input
                  id={emailId}
                  type="email"
                  name={field.name}
                  value={field.state.value}
                  onChange={(event) => field.handleChange(event.target.value)}
                  onBlur={field.handleBlur}
                  autoComplete="email"
                />
              </Field>
            )}
          </form.Field>
        </FieldGroup>
      </FieldSet>
      <CardFooter className="-mx-(--card-spacing) mt-6 -mb-(--card-spacing) justify-between gap-4">
        <p role="alert" className="text-destructive">
          {saveIdentity.isError ? errorMessage(saveIdentity.error) : null}
        </p>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <Button type="submit" loading={isSubmitting}>
              {GIT_DESCRIPTIONS.save}
            </Button>
          )}
        </form.Subscribe>
      </CardFooter>
    </form>
  );
}
