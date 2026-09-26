import type { ServerLogTail } from "@ezra/client";
import {
  getFolderRemoteControlLogOptions,
  getRemoteControlLogOptions,
} from "@ezra/client/react-query.gen";
import { useQuery } from "@tanstack/react-query";
import { useLayoutEffect, useRef } from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { REMOTE_CONTROL_DESCRIPTIONS } from "@/content/remote-control";
import { errorMessage } from "@/lib/utils";

/** Poll interval while the log is open. */
const LOG_POLL_MS = 5_000;
/** How close to the end the reader must be for new lines to scroll into view. */
const FOLLOW_SLACK_PX = 24;

export interface ServerLogDialogProps {
  /** The folder whose server's log to show, or none for the /projects server. */
  folder?: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** The last lines of a Remote Control server's log, kept current while open. */
export function ServerLogDialog({ folder, open, onOpenChange }: ServerLogDialogProps) {
  const log = useServerLog(folder, open);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent closeLabel={REMOTE_CONTROL_DESCRIPTIONS.close} className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>
            {folder === undefined
              ? REMOTE_CONTROL_DESCRIPTIONS.projects_log_title
              : REMOTE_CONTROL_DESCRIPTIONS.log_title(folder)}
          </DialogTitle>
          {log.data ? (
            <DialogDescription className="break-all">
              {REMOTE_CONTROL_DESCRIPTIONS.log_description(log.data.path)}
            </DialogDescription>
          ) : null}
        </DialogHeader>
        <LogBody log={log} />
      </DialogContent>
    </Dialog>
  );
}

function useServerLog(folder: string | undefined, open: boolean) {
  const projects = useQuery({
    ...getRemoteControlLogOptions(),
    enabled: open && folder === undefined,
    refetchInterval: LOG_POLL_MS,
  });
  const inFolder = useQuery({
    ...getFolderRemoteControlLogOptions({ path: { name: folder ?? "" } }),
    enabled: open && folder !== undefined,
    refetchInterval: LOG_POLL_MS,
  });
  return folder === undefined ? projects : inFolder;
}

function LogBody({ log }: { log: ReturnType<typeof useServerLog> }) {
  if (log.isPending) {
    return (
      <div className="flex justify-center text-muted-foreground">
        <Spinner />
      </div>
    );
  }
  if (log.isError) {
    return (
      <p role="alert" className="text-destructive">
        {errorMessage(log.error)}
      </p>
    );
  }
  if (log.data.lines.length === 0) {
    return <p className="text-muted-foreground">{REMOTE_CONTROL_DESCRIPTIONS.log_empty}</p>;
  }
  return <LogLines tail={log.data} />;
}

/** Follows new lines while the reader is at the end, and stays put once they scroll up. */
function LogLines({ tail }: { tail: ServerLogTail }) {
  const scroller = useRef<HTMLPreElement>(null);
  const following = useRef(true);
  useLayoutEffect(() => {
    const element = scroller.current;
    if (element && following.current) {
      element.scrollTop = element.scrollHeight;
    }
  });
  return (
    <pre
      ref={scroller}
      tabIndex={0}
      aria-label={REMOTE_CONTROL_DESCRIPTIONS.log_lines}
      onScroll={(event) => {
        const element = event.currentTarget;
        following.current =
          element.scrollHeight - element.scrollTop - element.clientHeight < FOLLOW_SLACK_PX;
      }}
      className="max-h-[60vh] overflow-auto rounded-md bg-muted p-3 font-mono text-xs break-all whitespace-pre-wrap outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
    >
      {tail.lines.join("\n")}
    </pre>
  );
}
