import type { Agent } from "@ezra/client";
import { useId, useState } from "react";

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
import { Field, FieldContent, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Switch } from "@/components/ui/switch";
import { AGENTS_DESCRIPTIONS, AGENT_NAMES } from "@/content/agents";
import { useAgentActions } from "@/hooks/use-agent-actions";
import { errorMessage } from "@/lib/utils";

export interface UninstallAgentDialogProps {
  agent: Agent;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Where focus goes once the dialog closes. */
  finalFocus: () => HTMLElement | null;
}

/** Confirms uninstalling an agent, and whether its saved data goes too. */
export function UninstallAgentDialog({
  agent,
  open,
  onOpenChange,
  finalFocus,
}: UninstallAgentDialogProps) {
  const { uninstall } = useAgentActions(agent);
  const [deleteData, setDeleteData] = useState(false);
  const ids = { data: useId(), dataLabel: useId(), dataHint: useId() };
  const name = AGENT_NAMES[agent];
  const close = () => {
    setDeleteData(false);
    uninstall.reset();
    onOpenChange(false);
  };
  return (
    <AlertDialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <AlertDialogContent finalFocus={finalFocus}>
        <AlertDialogHeader>
          <AlertDialogTitle>{AGENTS_DESCRIPTIONS.uninstall_title(name)}</AlertDialogTitle>
          <AlertDialogDescription>{AGENTS_DESCRIPTIONS.uninstall_body}</AlertDialogDescription>
        </AlertDialogHeader>
        <Field orientation="horizontal">
          <Switch
            id={ids.data}
            aria-labelledby={ids.dataLabel}
            aria-describedby={ids.dataHint}
            checked={deleteData}
            onCheckedChange={setDeleteData}
            disabled={uninstall.isPending}
          />
          <FieldContent>
            <FieldLabel id={ids.dataLabel} htmlFor={ids.data}>
              {AGENTS_DESCRIPTIONS.delete_saved_data}
            </FieldLabel>
            <FieldDescription id={ids.dataHint}>
              {AGENTS_DESCRIPTIONS.delete_saved_data_hint}
            </FieldDescription>
          </FieldContent>
        </Field>
        {uninstall.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(uninstall.error)}
          </p>
        ) : null}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={uninstall.isPending}>
            {AGENTS_DESCRIPTIONS.cancel}
          </AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            loading={uninstall.isPending}
            onClick={() =>
              uninstall.mutate(
                { path: { agent }, query: { saved_data: deleteData } },
                { onSuccess: close },
              )
            }
          >
            {AGENTS_DESCRIPTIONS.uninstall}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
