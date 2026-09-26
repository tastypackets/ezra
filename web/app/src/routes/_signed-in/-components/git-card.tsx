import type { CommitIdentity, GitHubStatus } from "@ezra/client";
import { useForm } from "@tanstack/react-form";
import { useSuspenseQuery } from "@tanstack/react-query";
import { cn } from "cn";

import { SignInSteps, WaitingForWebsite } from "@/components/sign-in-steps";
import { Badge } from "@/components/ui/badge";
import type { BadgeProps } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardFooter, CardHeader } from "@/components/ui/card";
import { Field, TextInput } from "@/components/ui/field";
import { GIT_DESCRIPTIONS } from "@/content/git";
import { useGitActions } from "@/hooks/use-git-actions";
import { errorMessage } from "@/lib/utils";
import { gitStatusQueryOptions } from "@/queries/git-queries";

/** The GitHub sign-in and commit identity every agent's git uses. */
export function GitCard() {
  const { data: git } = useSuspenseQuery(gitStatusQueryOptions);
  return (
    <Card>
      <CardHeader title={GIT_DESCRIPTIONS.title} description={GIT_DESCRIPTIONS.description} />
      <GitHubSection github={git.github} />
      <IdentityForm identity={git.identity} />
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
    <section className="border-b border-ez-border">
      <div className="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
        <div className="flex flex-wrap items-center gap-3">
          <h3 className="font-medium">{GIT_DESCRIPTIONS.github}</h3>
          <Badge tone={state.tone}>{state.label}</Badge>
        </div>
        {canSignOut ? (
          <Button
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
            variant="primary"
            loading={startGitHubSignIn.isPending}
            onClick={() => startGitHubSignIn.mutate()}
          >
            {GIT_DESCRIPTIONS.sign_in}
          </Button>
        ) : null}
      </div>
      {github.from_environment || github.failing || failed ? (
        <div className="-mt-2 flex flex-col gap-1 px-5 pb-4">
          {github.from_environment ? (
            <p className="text-ez-muted">{GIT_DESCRIPTIONS.from_environment}</p>
          ) : null}
          {github.failing ? <p className="text-ez-danger">{GIT_DESCRIPTIONS.failing}</p> : null}
          {failed ? (
            <p role="alert" className="text-ez-danger">
              {errorMessage(failed.error)}
            </p>
          ) : null}
        </div>
      ) : null}
      {github.login_prompt ? (
        <div className="border-t border-ez-border">
          <SignInSteps prompt={github.login_prompt} />
          <div className="flex items-center justify-between gap-4 px-5 pb-4">
            <WaitingForWebsite />
            <Button
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

function gitHubState(github: GitHubStatus): { label: string; tone: BadgeProps["tone"] } {
  if (github.login_prompt) {
    return { label: GIT_DESCRIPTIONS.signing_in, tone: "pending" };
  }
  if (github.signed_in) {
    return {
      label: github.account
        ? GIT_DESCRIPTIONS.signed_in_as(github.account)
        : GIT_DESCRIPTIONS.signed_in,
      tone: "good",
    };
  }
  if (github.failing) {
    return { label: GIT_DESCRIPTIONS.not_confirmed, tone: "pending" };
  }
  return { label: GIT_DESCRIPTIONS.signed_out, tone: "neutral" };
}

function IdentityForm({ identity }: { identity: CommitIdentity }) {
  const { saveIdentity } = useGitActions();
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
      <div className="flex flex-col gap-3 px-5 py-4">
        <h3 className="font-medium">{GIT_DESCRIPTIONS.identity}</h3>
        <div className="grid gap-3 sm:grid-cols-2">
          <form.Field name="name">
            {(field) => (
              <Field label={GIT_DESCRIPTIONS.name}>
                <TextInput
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
              <Field label={GIT_DESCRIPTIONS.email}>
                <TextInput
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
        </div>
      </div>
      <CardFooter>
        <form.Subscribe selector={(state) => state.isDefaultValue}>
          {(unchanged) => (
            <p
              role="status"
              className={cn(
                saveIdentity.isError ? "text-[0.8125rem] text-ez-danger" : "text-ez-muted",
              )}
            >
              {saveIdentity.isError
                ? errorMessage(saveIdentity.error)
                : saveIdentity.isSuccess && unchanged
                  ? GIT_DESCRIPTIONS.saved
                  : null}
            </p>
          )}
        </form.Subscribe>
        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <Button type="submit" variant="primary" loading={isSubmitting}>
              {GIT_DESCRIPTIONS.save}
            </Button>
          )}
        </form.Subscribe>
      </CardFooter>
    </form>
  );
}
