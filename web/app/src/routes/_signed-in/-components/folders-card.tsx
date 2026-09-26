import type { FolderStatus, RemoteControlStatus } from "@ezra/client";
import { useQuery } from "@tanstack/react-query";
import { ExternalLinkIcon } from "lucide-react";
import prettyBytes from "pretty-bytes";
import { useId } from "react";

import { Badge } from "@/components/ui/badge";
import { buttonVariants } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Hint } from "@/components/ui/hint";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { FOLDERS_DESCRIPTIONS } from "@/content/folders";
import { REMOTE_CONTROL_DESCRIPTIONS, SERVER_STATES } from "@/content/remote-control";
import { useFolderActions } from "@/hooks/use-folder-actions";
import { errorMessage } from "@/lib/utils";
import { foldersQueryOptions } from "@/queries/folder-queries";
import { remoteControlQueryOptions } from "@/queries/remote-control-queries";

import { SERVER_BADGES } from "./remote-control-card";

/** The projects agents work in, and which ones the Claude app lists. */
export function FoldersCard() {
  const folders = useQuery(foldersQueryOptions);
  const remoteControl = useQuery(remoteControlQueryOptions);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{FOLDERS_DESCRIPTIONS.title}</CardTitle>
      </CardHeader>
      <CardContent>
        {folders.isPending ? (
          <div className="flex justify-center text-muted-foreground">
            <Spinner />
          </div>
        ) : folders.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(folders.error)}
          </p>
        ) : folders.data.length === 0 ? (
          <p className="text-muted-foreground">{FOLDERS_DESCRIPTIONS.empty}</p>
        ) : (
          <ul className="flex flex-col divide-y">
            {folders.data.map((folder) => (
              <FolderRow
                key={folder.name}
                folder={folder}
                server={remoteControl.data?.folders[folder.name]}
              />
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

function FolderRow({ folder, server }: { folder: FolderStatus; server?: RemoteControlStatus }) {
  const { chooseToServe } = useFolderActions();
  const detail = folder.git ? folder.git.repository : FOLDERS_DESCRIPTIONS.not_git;
  const switchId = useId();
  const switchLabelId = useId();
  return (
    <li className="flex flex-col gap-2 py-3 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex min-w-0 flex-col gap-0.5">
        <div className="flex flex-wrap items-baseline gap-x-3">
          <span className="min-w-0 font-medium break-all">{folder.name}</span>
          {folder.git?.branch ? (
            <span className="min-w-0 font-mono text-xs break-all text-muted-foreground">
              {folder.git.branch}
            </span>
          ) : null}
        </div>
        {detail ? <span className="truncate text-muted-foreground">{detail}</span> : null}
        {chooseToServe.isError ? (
          <p role="alert" className="text-destructive">
            {errorMessage(chooseToServe.error)}
          </p>
        ) : null}
      </div>
      <div className="flex flex-none items-center gap-3">
        {server?.usage ? (
          <span className="flex gap-3 text-xs text-muted-foreground tabular-nums">
            {REMOTE_CONTROL_DESCRIPTIONS.sessions(server.usage.sessions, server.usage.capacity)}
            <Hint content={REMOTE_CONTROL_DESCRIPTIONS.memory_hint}>
              {prettyBytes(server.usage.memory_bytes)}
            </Hint>
          </span>
        ) : null}
        {server ? (
          <Badge variant={SERVER_BADGES[server.state]}>{SERVER_STATES[server.state]}</Badge>
        ) : null}
        {server?.url ? (
          <a
            href={server.url}
            target="_blank"
            rel="noopener noreferrer"
            aria-label={REMOTE_CONTROL_DESCRIPTIONS.open}
            className={buttonVariants({ variant: "ghost", size: "icon-sm" })}
          >
            <ExternalLinkIcon />
          </a>
        ) : null}
        <div className="flex items-center gap-2">
          <Switch
            id={switchId}
            checked={chooseToServe.isPending ? chooseToServe.variables.body.serve : folder.serve}
            disabled={chooseToServe.isPending}
            onCheckedChange={(serve) =>
              chooseToServe.mutate({ path: { name: folder.name }, body: { serve } })
            }
            aria-labelledby={switchLabelId}
          />
          <span id={switchLabelId} className="sr-only">
            {FOLDERS_DESCRIPTIONS.serve_label(folder.name)}
          </span>
          <Label htmlFor={switchId} className="text-muted-foreground">
            {FOLDERS_DESCRIPTIONS.serve}
          </Label>
        </div>
      </div>
    </li>
  );
}
