import type { RemoteControlOverview, RemoteControlStatus, ServerState } from "@ezra/client";
import { Link } from "@tanstack/react-router";
import { ExternalLinkIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";

import { Badge } from "@/components/ui/badge";
import type { badgeVariants } from "@/components/ui/badge";
import { buttonVariants } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";

type BadgeVariant = NonNullable<Parameters<typeof badgeVariants>[0]>["variant"];

export const SERVER_BADGES: Record<ServerState, BadgeVariant> = {
  off: "secondary",
  waiting: "secondary",
  starting: "warning",
  running: "success",
  retrying: "warning",
  stopping: "warning",
};

/** Claude Code's Remote Control server on /projects: whether it runs, and where to continue. */
export function RemoteControlCard({ overview }: { overview: RemoteControlOverview }) {
  const status = overview.projects;
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          {REMOTE_CONTROL_DESCRIPTIONS.title}
          <Badge variant={SERVER_BADGES[status.state]}>{SERVER_STATES[status.state]}</Badge>
        </CardTitle>
        {status.url ? (
          <CardAction>
            <a
              href={status.url}
              target="_blank"
              rel="noopener noreferrer"
              className={buttonVariants({ variant: "outline", size: "sm" })}
            >
              {REMOTE_CONTROL_DESCRIPTIONS.open}
              <ExternalLinkIcon data-icon="inline-end" />
            </a>
          </CardAction>
        ) : null}
      </CardHeader>
      <CardContent className="flex flex-col gap-2">
        <StateHint status={status} device={overview.device} />
        {status.usage ? (
          <p className="text-muted-foreground">
            {REMOTE_CONTROL_DESCRIPTIONS.usage(
              status.usage.sessions,
              status.usage.capacity,
              prettyBytes(status.usage.memory_bytes),
            )}
          </p>
        ) : null}
        {status.state === "retrying" && status.last_error ? (
          <p className="text-destructive">
            {REMOTE_CONTROL_DESCRIPTIONS.last_stop}:{" "}
            <span className="font-mono text-xs break-words whitespace-pre-wrap">
              {status.last_error}
            </span>
          </p>
        ) : null}
      </CardContent>
    </Card>
  );
}

function StateHint({ status, device }: { status: RemoteControlStatus; device?: string | null }) {
  switch (status.state) {
    case "off":
      return (
        <p className="text-muted-foreground">
          <Link to="/settings" className="underline underline-offset-4 hover:text-foreground">
            {REMOTE_CONTROL_DESCRIPTIONS.off_hint}
          </Link>
        </p>
      );
    case "running":
      return (
        <p className="text-muted-foreground">
          {device
            ? REMOTE_CONTROL_DESCRIPTIONS.running_hint(device)
            : REMOTE_CONTROL_DESCRIPTIONS.running_hint_without_device}
        </p>
      );
    case "waiting":
      return <p className="text-muted-foreground">{REMOTE_CONTROL_DESCRIPTIONS.waiting_hint}</p>;
    case "starting":
      return <p className="text-muted-foreground">{REMOTE_CONTROL_DESCRIPTIONS.starting_hint}</p>;
    default:
      return null;
  }
}
