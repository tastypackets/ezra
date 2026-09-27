import { useQuery } from "@tanstack/react-query";

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
import { REMOTE_CONTROL_DESCRIPTIONS } from "@/content/remote-control";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

export interface CodexSignInDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}

/** Asks before a Codex sign-in ends the current one. */
export function CodexSignInDialog({ open, onOpenChange, onConfirm }: CodexSignInDialogProps) {
  const { data: chatsRunning = false } = useQuery({
    ...remoteControlQueryOptions,
    select: (overview) => (overview.codex.usage?.running_chats ?? 0) > 0,
  });
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>
            {REMOTE_CONTROL_DESCRIPTIONS.codex_sign_in_again_title}
          </AlertDialogTitle>
          <AlertDialogDescription>
            {REMOTE_CONTROL_DESCRIPTIONS.codex_sign_in_again_body(chatsRunning)}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{REMOTE_CONTROL_DESCRIPTIONS.cancel}</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm}>
            {REMOTE_CONTROL_DESCRIPTIONS.sign_in_again}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
