import { useQueryErrorResetBoundary } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";
import type { ErrorComponentProps } from "@tanstack/react-router";
import { useEffect } from "react";

import { StateCard } from "@/components/state-card";
import { Button } from "@/components/ui/button";
import { APP_DESCRIPTIONS } from "@/content/app";
import { errorMessage } from "@/lib/utils";

/** What a route shows when its guard or loader fails, with a retry. */
export function ErrorState({ error }: ErrorComponentProps) {
  const router = useRouter();
  const queryErrorResetBoundary = useQueryErrorResetBoundary();
  useEffect(() => {
    queryErrorResetBoundary.reset();
  }, [queryErrorResetBoundary]);
  return (
    <StateCard
      title={APP_DESCRIPTIONS.error_title}
      description={errorMessage(error)}
      action={
        <Button variant="primary" onClick={() => void router.invalidate()}>
          {APP_DESCRIPTIONS.retry}
        </Button>
      }
    />
  );
}
