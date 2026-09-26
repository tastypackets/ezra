import type { Agent, LoginPrompt } from "@ezra/client";
import { useForm } from "@tanstack/react-form";
import { ExternalLink } from "lucide-react";

import { Button, buttonClassName } from "@/components/ui/button";
import { Card, CardFooter, CardHeader } from "@/components/ui/card";
import { CopyButton } from "@/components/ui/copy-button";
import { Field, TextInput } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import { useAgentActions } from "@/hooks/use-agent-actions";

export interface SignInPanelProps {
  agent: Agent;
  prompt: LoginPrompt;
}

/** The steps for an agent sign-in that is waiting on the person signing in. */
export function SignInPanel({ agent, prompt }: SignInPanelProps) {
  const { startSignIn } = useAgentActions(agent);
  return (
    <Card>
      <CardHeader
        title={AGENTS_DESCRIPTIONS.sign_in_title(AGENT_NAMES[agent])}
        description={AGENTS_DESCRIPTIONS.sign_in_description}
      />
      <ol className="divide-y divide-ez-border px-5">
        <Step number={1} title={AGENTS_DESCRIPTIONS.step_open}>
          <a
            href={prompt.url}
            target="_blank"
            rel="noopener noreferrer"
            className={buttonClassName({ size: "sm" })}
          >
            {siteOf(prompt.url)}
            <ExternalLink aria-hidden="true" className="size-3.5" />
          </a>
        </Step>
        {prompt.code ? (
          <Step number={2} title={AGENTS_DESCRIPTIONS.step_enter_code}>
            <div className="flex items-center gap-3">
              <span className="rounded-md border border-dashed border-ez-border-strong bg-ez-surface-muted px-3 py-1 font-mono text-xl font-semibold tracking-widest">
                {prompt.code}
              </span>
              <CopyButton
                text={prompt.code}
                label={AGENTS_DESCRIPTIONS.copy}
                copiedLabel={AGENTS_DESCRIPTIONS.copied}
              />
            </div>
          </Step>
        ) : (
          <Step number={2} title={AGENTS_DESCRIPTIONS.step_paste_code}>
            <CodeForm key={prompt.url} agent={agent} />
          </Step>
        )}
      </ol>
      <CardFooter>
        {prompt.code ? (
          <p className="flex items-center gap-2 text-ez-muted">
            <Spinner className="text-ez-accent" />
            {AGENTS_DESCRIPTIONS.waiting_for_website}
          </p>
        ) : (
          <span />
        )}
        <Button size="sm" loading={startSignIn.isPending} onClick={() => startSignIn.mutate()}>
          {AGENTS_DESCRIPTIONS.start_over}
        </Button>
      </CardFooter>
    </Card>
  );
}

function Step({
  number,
  title,
  children,
}: {
  number: number;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <li className="flex gap-3.5 py-3">
      <span className="inline-flex size-6 flex-none items-center justify-center rounded-full bg-ez-neutral-soft text-xs font-semibold text-ez-neutral">
        {number}
      </span>
      <div className="flex flex-col gap-2">
        <p className="font-medium">{title}</p>
        {children}
      </div>
    </li>
  );
}

function CodeForm({ agent }: { agent: Agent }) {
  const { submitCode } = useAgentActions(agent);
  const form = useForm({
    defaultValues: { code: "" },
    onSubmit: async ({ value }) => {
      await submitCode.mutateAsync(value.code).catch(() => undefined);
    },
  });
  return (
    <form
      className="flex max-w-md flex-wrap items-start gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (!form.state.isSubmitting) {
          void form.handleSubmit();
        }
      }}
    >
      <form.Field name="code">
        {(field) => (
          <Field label={AGENTS_DESCRIPTIONS.code_label} hideLabel className="min-w-48 flex-1">
            <TextInput
              name={field.name}
              value={field.state.value}
              onChange={(event) => field.handleChange(event.target.value)}
              onBlur={field.handleBlur}
              placeholder={AGENTS_DESCRIPTIONS.code_placeholder}
              autoComplete="off"
              spellCheck={false}
              required
            />
          </Field>
        )}
      </form.Field>
      <form.Subscribe selector={(state) => state.isSubmitting}>
        {(isSubmitting) => (
          <Button type="submit" variant="primary" loading={isSubmitting}>
            {AGENTS_DESCRIPTIONS.finish_sign_in}
          </Button>
        )}
      </form.Subscribe>
    </form>
  );
}

/** The host a sign-in link points at, for the button label, e.g. `claude.com`. */
function siteOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
