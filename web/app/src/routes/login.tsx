import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";

import { AppHeader } from "@/components/app-header";
import { PasswordForm } from "@/components/password-form";
import { SESSION_DESCRIPTIONS } from "@/content/session";
import { useSessionActions } from "@/hooks/use-session-actions";
import { sessionQueryOptions } from "@/queries/session-queries";

export const Route = createFileRoute("/login")({
  beforeLoad: async ({ context }) => {
    const session = await context.queryClient.ensureQueryData(sessionQueryOptions);
    if (!session.claimed) {
      throw redirect({ to: "/setup" });
    }
    if (session.authenticated) {
      throw redirect({ to: "/" });
    }
  },
  component: LoginPage,
});

function LoginPage() {
  const navigate = useNavigate();
  const { signIn } = useSessionActions();
  return (
    <>
      <AppHeader signedIn={false} />
      <PasswordForm
        title={SESSION_DESCRIPTIONS.login_title}
        description={SESSION_DESCRIPTIONS.login_description}
        submitLabel={SESSION_DESCRIPTIONS.login_submit}
        autoComplete="current-password"
        mutation={signIn}
        onSuccess={() => void navigate({ to: "/" })}
      />
    </>
  );
}
