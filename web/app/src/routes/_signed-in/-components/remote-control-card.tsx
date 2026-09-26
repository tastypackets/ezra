import type { RemoteControlStatus } from "@ezra/client";
import { Link } from "@tanstack/react-router";
import { ExternalLink } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import type { BadgeProps } from "@/components/ui/badge";
import { buttonClassName } from "@/components/ui/button";
import { Card, CardHeader } from "@/components/ui/card";
import { AGENT_NAMES } from "@/content/agents";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";

const TONES: Record<RemoteControlStatus["state"], BadgeProps["tone"]> = {
  off: "neutral",
  waiting: "neutral",
  starting: "pending",
  running: "good",
  retrying: "pending",
};

/** Claude Code's Remote Control server: whether it runs, and where to continue. */
export function RemoteControlCard({ status }: { status: RemoteControlStatus }) {
  return (
    <Card>
      <CardHeader
        title={REMOTE_CONTROL_DESCRIPTIONS.title}
        description={REMOTE_CONTROL_DESCRIPTIONS.description}
      />
      <div className="flex flex-col gap-2 px-5 py-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex flex-wrap items-center gap-3">
            <span className="font-medium">{AGENT_NAMES.claude}</span>
            <Badge tone={TONES[status.state]}>{SERVER_STATES[status.state]}</Badge>
          </div>
          {status.url ? (
            <a
              href={status.url}
              target="_blank"
              rel="noopener noreferrer"
              className={buttonClassName({ size: "sm" })}
            >
              {REMOTE_CONTROL_DESCRIPTIONS.open}
              <ExternalLink aria-hidden="true" className="size-3.5" />
            </a>
          ) : null}
        </div>
        <StateHint status={status} />
        {status.state === "retrying" && status.last_error ? (
          <p className="text-ez-danger">
            {REMOTE_CONTROL_DESCRIPTIONS.last_stop}:{" "}
            <span className="font-mono text-[0.8125rem] break-words whitespace-pre-wrap">
              {status.last_error}
            </span>
          </p>
        ) : null}
      </div>
    </Card>
  );
}

const HINTS: Record<RemoteControlStatus["state"], string | null> = {
  off: null,
  waiting: REMOTE_CONTROL_DESCRIPTIONS.waiting_hint,
  starting: REMOTE_CONTROL_DESCRIPTIONS.starting_hint,
  running: REMOTE_CONTROL_DESCRIPTIONS.running_hint,
  retrying: null,
};

function StateHint({ status }: { status: RemoteControlStatus }) {
  if (status.state === "off") {
    return (
      <p className="text-ez-muted">
        <Link to="/settings" className="underline underline-offset-4 hover:text-ez-text">
          {REMOTE_CONTROL_DESCRIPTIONS.off_hint}
        </Link>
      </p>
    );
  }
  const hint = HINTS[status.state];
  return hint ? <p className="text-ez-muted">{hint}</p> : null;
}
