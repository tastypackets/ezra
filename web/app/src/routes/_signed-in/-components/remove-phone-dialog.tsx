import type { PairedPhone } from "@ezra/client";
import { useRef } from "react";
import type { RefObject } from "react";

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
import { PAIRING_DESCRIPTIONS } from "@/content/pairing";
import { REMOTE_CONTROL_DESCRIPTIONS } from "@/content/remote-control";
import { useCodexActions } from "@/hooks/use-codex-actions";
import { phoneName } from "@/lib/pairing";
import { errorMessage } from "@/lib/utils";

export interface RemovePhoneDialogProps {
  phone: PairedPhone;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Where focus goes once the phone is removed. */
  afterRemoval: RefObject<HTMLElement | null>;
}

/** Confirms removing a paired phone from Codex. */
export function RemovePhoneDialog({
  phone,
  open,
  onOpenChange,
  afterRemoval,
}: RemovePhoneDialogProps) {
  const { removePhone } = useCodexActions();
  const removed = useRef(false);
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent finalFocus={() => (removed.current ? afterRemoval.current : true)}>
        <AlertDialogHeader>
          <AlertDialogTitle>{PAIRING_DESCRIPTIONS.remove_title(phoneName(phone))}</AlertDialogTitle>
          <AlertDialogDescription>{PAIRING_DESCRIPTIONS.remove_body}</AlertDialogDescription>
        </AlertDialogHeader>
        {removePhone.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(removePhone.error)}
          </p>
        ) : null}
        <AlertDialogFooter>
          <AlertDialogCancel>{REMOTE_CONTROL_DESCRIPTIONS.cancel}</AlertDialogCancel>
          <AlertDialogAction
            variant="destructive"
            loading={removePhone.isPending}
            onClick={() =>
              removePhone.mutate(
                { path: { id: phone.id } },
                {
                  onSuccess: () => {
                    removed.current = true;
                    onOpenChange(false);
                  },
                },
              )
            }
          >
            {PAIRING_DESCRIPTIONS.remove_confirm}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
