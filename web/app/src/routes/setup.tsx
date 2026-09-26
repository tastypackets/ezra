import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";

import { PasswordForm } from "@/components/password-form";
import { SESSION_DESCRIPTIONS } from "@/content/session";
import { useSessionActions } from "@/hooks/use-session-actions";
import { sessionQueryOptions } from "@/queries/session-queries";

export const Route = createFileRoute("/setup")({
  beforeLoad: async ({ context }) => {
    const session = await context.queryClient.ensureQueryData(sessionQueryOptions);
    if (session.claimed) {
      throw redirect({ to: "/" });
    }
  },
  component: SetupPage,
});

function SetupPage() {
  const navigate = useNavigate();
  const { setUp } = useSessionActions();
  return (
    <PasswordForm
      title={SESSION_DESCRIPTIONS.setup_title}
      description={SESSION_DESCRIPTIONS.setup_description}
      submitLabel={SESSION_DESCRIPTIONS.setup_submit}
      autoComplete="new-password"
      submit={(password) => setUp.mutateAsync({ body: { password } })}
      error={setUp.error}
      onSuccess={() => navigate({ to: "/" })}
    />
  );
}
