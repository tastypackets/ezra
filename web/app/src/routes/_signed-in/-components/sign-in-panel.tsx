import type { Agent, LoginPrompt } from "@ezra/client";
import { useForm } from "@tanstack/react-form";

import { SignInSteps, WaitingForWebsite } from "@/components/sign-in-steps";
import { Button } from "@/components/ui/button";
import { Card, CardFooter, CardHeader } from "@/components/ui/card";
import { Field, TextInput } from "@/components/ui/field";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import { useAgentActionPending, useAgentActions } from "@/hooks/use-agent-actions";

export interface SignInPanelProps {
  agent: Agent;
  prompt: LoginPrompt;
}

/** The steps for an agent sign-in that is waiting on the person signing in. */
export function SignInPanel({ agent, prompt }: SignInPanelProps) {
  const { startSignIn } = useAgentActions(agent);
  const restarting = useAgentActionPending(agent, "start_sign_in");
  return (
    <Card>
      <CardHeader
        title={AGENTS_DESCRIPTIONS.sign_in_title(AGENT_NAMES[agent])}
        description={AGENTS_DESCRIPTIONS.sign_in_description}
      />
      <SignInSteps prompt={prompt} codeForm={<CodeForm key={prompt.url} agent={agent} />} />
      <CardFooter>
        {prompt.code ? <WaitingForWebsite /> : <span />}
        <Button size="sm" loading={restarting} onClick={() => startSignIn.mutate()}>
          {AGENTS_DESCRIPTIONS.start_over}
        </Button>
      </CardFooter>
    </Card>
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
