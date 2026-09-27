import type { Agent } from "@ezra/client";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { AGENTS_DESCRIPTIONS } from "@/content/agents";
import { useAgentActions } from "@/hooks/use-agent-actions";
import { errorMessage } from "@/lib/utils";

export interface RestartServersDialogProps {
  agent: Agent;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Where focus goes once the dialog closes. */
  finalFocus: () => HTMLElement | null;
}

/** Confirms restarting an agent's servers, which ends what runs in them. */
export function RestartServersDialog({
  agent,
  open,
  onOpenChange,
  finalFocus,
}: RestartServersDialogProps) {
  const { restartServers } = useAgentActions(agent);
  const close = () => {
    restartServers.reset();
    onOpenChange(false);
  };
  return (
    <AlertDialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <AlertDialogContent finalFocus={finalFocus}>
        <AlertDialogHeader>
          <AlertDialogTitle>{AGENTS_DESCRIPTIONS.restart_title[agent]}</AlertDialogTitle>
          <AlertDialogDescription>{AGENTS_DESCRIPTIONS.restart_body[agent]}</AlertDialogDescription>
        </AlertDialogHeader>
        {restartServers.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(restartServers.error)}
          </p>
        ) : null}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={restartServers.isPending}>
            {AGENTS_DESCRIPTIONS.cancel}
          </AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            loading={restartServers.isPending}
            onClick={() => restartServers.mutate({ path: { agent } }, { onSuccess: close })}
          >
            {AGENTS_DESCRIPTIONS.restart_servers[agent]}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
