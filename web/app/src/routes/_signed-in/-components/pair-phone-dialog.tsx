import type { CodexPairing } from "@ezra/client";
import { useQuery } from "@tanstack/react-query";
import { ChevronDownIcon, ExternalLinkIcon } from "lucide-react";
import { useCallback, useState } from "react";

import { CodeToEnter, Step, Waiting } from "@/components/sign-in-steps";
import { Button, buttonVariants } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { PAIRING_DESCRIPTIONS } from "@/content/pairing";
import { REMOTE_CONTROL_DESCRIPTIONS } from "@/content/remote-control";
import type { CodexPairingFlow } from "@/hooks/use-codex-actions";
import { useNow } from "@/hooks/use-now";
import type { RemoteView } from "@/lib/codex-remote";
import { handOffFocus } from "@/lib/focus";
import { countdown } from "@/lib/pairing";
import { errorMessage } from "@/lib/utils";
import { codexPairingQrUrl, remoteControlQueryOptions } from "@/queries/remote-control-queries";

/** How often the time left on a code is read again. */
const COUNTDOWN_MS = 1_000;

export interface PairPhoneDialogProps {
  flow: CodexPairingFlow;
}

/** The code a phone pairs with Codex through, or why there is none. */
export function PairPhoneDialog({ flow }: PairPhoneDialogProps) {
  return (
    <Dialog open={flow.open} onOpenChange={flow.setOpen}>
      <DialogContent
        closeLabel={REMOTE_CONTROL_DESCRIPTIONS.close}
        className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
      >
        <DialogHeader>
          <DialogTitle>{PAIRING_DESCRIPTIONS.pair}</DialogTitle>
        </DialogHeader>
        {flow.unavailable ? <Unavailable remote={flow.unavailable} /> : <PairingCode flow={flow} />}
      </DialogContent>
    </Dialog>
  );
}

/** Why Codex cannot pair: its Remote cell's label and problem note. */
function Unavailable({ remote }: { remote: RemoteView }) {
  return (
    <DialogDescription render={<div />} className="flex flex-col gap-1 text-foreground">
      <p className="font-medium">{remote.label}</p>
      {remote.note?.tone === "destructive" ? (
        <p className="text-destructive">{remote.note.text}</p>
      ) : null}
      <p className="text-muted-foreground">{PAIRING_DESCRIPTIONS.pair_unavailable}</p>
    </DialogDescription>
  );
}

/** The code with what the phone needs, or the expired or failed code with a way to a new one. */
function PairingCode({ flow: { start, pairing, newCode } }: PairPhoneDialogProps) {
  const { data: codex } = useQuery({
    ...remoteControlQueryOptions,
    select: (overview) => overview.codex,
  });
  const keepFocusInDialog = useCallback((button: HTMLButtonElement | null) => {
    const dialog = button?.closest<HTMLElement>('[role="dialog"]');
    return handOffFocus(button, () => dialog?.querySelector("button"));
  }, []);
  const failure = start.isError
    ? { title: PAIRING_DESCRIPTIONS.pair_failed, text: errorMessage(start.error) }
    : pairing?.state === "failed"
      ? { title: PAIRING_DESCRIPTIONS.check_failed, text: pairing.error ?? "" }
      : undefined;
  const usable =
    !start.isPending && failure === undefined && pairing?.state === "open" ? pairing : undefined;
  const expired = failure === undefined && pairing?.state === "expired";
  return (
    <div className="flex min-w-0 flex-col gap-4">
      {usable?.manual_code ? (
        <ol>
          <Step number={1} title={PAIRING_DESCRIPTIONS.pair_steps}>
            <CodeToEnter code={usable.manual_code} />
          </Step>
        </ol>
      ) : null}
      {usable ? (
        <>
          <Countdown expiresAt={usable.expires_at} />
          <Waiting label={PAIRING_DESCRIPTIONS.waiting_for_phone} />
        </>
      ) : null}
      <DialogDescription render={<div />} role="status" className="text-foreground empty:sr-only">
        {failure ? (
          <p className="text-destructive">
            {failure.title}:{" "}
            <span className="font-mono text-xs break-words whitespace-pre-wrap">
              {failure.text}
            </span>
          </p>
        ) : null}
        {expired ? <p>{PAIRING_DESCRIPTIONS.expired}</p> : null}
      </DialogDescription>
      {failure || expired || start.isPending ? (
        <Button
          ref={keepFocusInDialog}
          className="self-start"
          loading={start.isPending}
          onClick={newCode}
        >
          {PAIRING_DESCRIPTIONS.new_code}
        </Button>
      ) : null}
      {usable ? (
        <>
          {codex?.server_name ? <p>{PAIRING_DESCRIPTIONS.listed_as(codex.server_name)}</p> : null}
          <p className="text-muted-foreground">{PAIRING_DESCRIPTIONS.clock_hint}</p>
          {codex?.folder_picker === "blocked" ? (
            <p className="text-muted-foreground">{PAIRING_DESCRIPTIONS.picker_blocked}</p>
          ) : null}
          <PairingQr pairing={usable} />
        </>
      ) : null}
    </div>
  );
}

function Countdown({ expiresAt }: { expiresAt: string }) {
  const now = useNow(COUNTDOWN_MS);
  return (
    <p className="text-muted-foreground tabular-nums">
      {PAIRING_DESCRIPTIONS.expires_in(countdown(expiresAt, now))}
    </p>
  );
}

/** The QR code behind a toggle next to a manual code, else on its own. */
function PairingQr({ pairing }: { pairing: CodexPairing }) {
  const [open, setOpen] = useState(false);
  if (!pairing.manual_code) {
    return (
      <div className="flex flex-col items-start gap-3">
        <QrCode pairing={pairing} />
      </div>
    );
  }
  return (
    <Collapsible open={open} onOpenChange={setOpen} className="flex flex-col items-start gap-3">
      <CollapsibleTrigger render={<Button variant="outline" size="sm" />}>
        {open ? PAIRING_DESCRIPTIONS.hide_qr : PAIRING_DESCRIPTIONS.show_qr}
        <ChevronDownIcon
          data-icon="inline-end"
          className="transition-transform group-data-panel-open/button:rotate-180"
        />
      </CollapsibleTrigger>
      <CollapsibleContent className="flex flex-col items-start gap-3">
        <QrCode pairing={pairing} />
      </CollapsibleContent>
    </Collapsible>
  );
}

/** The link as a QR code for the phone's camera, and as a link for a phone showing this page. */
function QrCode({ pairing }: { pairing: CodexPairing }) {
  return (
    <>
      <img
        src={codexPairingQrUrl(pairing.expires_at)}
        alt={PAIRING_DESCRIPTIONS.qr_alt}
        className="size-48 rounded-md"
      />
      <p className="text-muted-foreground">{PAIRING_DESCRIPTIONS.qr_hint}</p>
      <a
        href={pairing.link}
        target="_blank"
        rel="noopener noreferrer"
        className={buttonVariants({ variant: "outline", size: "sm" })}
      >
        {PAIRING_DESCRIPTIONS.open_in_app}
        <ExternalLinkIcon data-icon="inline-end" />
      </a>
    </>
  );
}
