import type { ErrorBody, PairedPhone } from "@ezra/client";
import { useQuery } from "@tanstack/react-query";
import type { UseQueryResult } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";
import type { RefObject } from "react";

import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { PAIRING_DESCRIPTIONS } from "@/content/pairing";
import { REMOTE_CONTROL_DESCRIPTIONS } from "@/content/remote-control";
import { phoneName } from "@/lib/pairing";
import { errorMessage, formatDateTime } from "@/lib/utils";
import { codexPhonesQueryOptions } from "@/queries/remote-control-queries";

import { RemovePhoneDialog } from "./remove-phone-dialog";

export interface PairedPhonesDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** The phones paired with Codex, each with a button that removes it. */
export function PairedPhonesDialog({ open, onOpenChange }: PairedPhonesDialogProps) {
  const heading = useRef<HTMLHeadingElement>(null);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        closeLabel={REMOTE_CONTROL_DESCRIPTIONS.close}
        className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
      >
        <DialogHeader>
          <DialogTitle ref={heading} tabIndex={-1} className="outline-none">
            {PAIRING_DESCRIPTIONS.phones}
          </DialogTitle>
        </DialogHeader>
        <PhoneList heading={heading} />
      </DialogContent>
    </Dialog>
  );
}

function PhoneList({ heading }: { heading: RefObject<HTMLHeadingElement | null> }) {
  const phones = useQuery(codexPhonesQueryOptions);
  const [removing, setRemoving] = useState<PairedPhone>();
  const [confirming, setConfirming] = useState(false);
  return (
    <>
      <PhoneListBody
        phones={phones}
        onRemove={(phone) => {
          setRemoving(phone);
          setConfirming(true);
        }}
      />
      {removing ? (
        <RemovePhoneDialog
          key={removing.id}
          phone={removing}
          open={confirming}
          onOpenChange={setConfirming}
          afterRemoval={heading}
        />
      ) : null}
    </>
  );
}

function PhoneListBody({
  phones,
  onRemove,
}: {
  phones: UseQueryResult<PairedPhone[], ErrorBody>;
  onRemove: (phone: PairedPhone) => void;
}) {
  if (phones.isPending) {
    return (
      <div className="flex justify-center text-muted-foreground">
        <Spinner />
      </div>
    );
  }
  if (phones.isError) {
    return (
      <p role="alert" className="text-destructive">
        {errorMessage(phones.error)}
      </p>
    );
  }
  if (phones.data.length === 0) {
    return <p className="text-muted-foreground">{PAIRING_DESCRIPTIONS.phones_empty}</p>;
  }
  return (
    <ul className="flex flex-col divide-y">
      {phones.data.map((phone) => (
        <Phone key={phone.id} phone={phone} onRemove={() => onRemove(phone)} />
      ))}
    </ul>
  );
}

function Phone({ phone, onRemove }: { phone: PairedPhone; onRemove: () => void }) {
  const nameId = useId();
  return (
    <li className="flex items-center justify-between gap-3 py-3 first:pt-0 last:pb-0">
      <div className="flex min-w-0 flex-col">
        <span id={nameId} className="truncate font-medium">
          {phoneName(phone)}
        </span>
        {phone.last_seen_at ? (
          <span className="text-muted-foreground">
            {PAIRING_DESCRIPTIONS.last_seen(formatDateTime(phone.last_seen_at))}
          </span>
        ) : null}
      </div>
      <Button variant="outline" size="sm" aria-describedby={nameId} onClick={onRemove}>
        {PAIRING_DESCRIPTIONS.remove}
      </Button>
    </li>
  );
}
