import type {
  Agent,
  GetSettingsFileError,
  ParseProblem,
  SettingsFileFormat,
  SettingsFileText,
} from "@ezra/client";
import { updateSettingsFileMutation } from "@ezra/client/react-query.gen";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { lazy, Suspense, useCallback, useId, useRef, useState } from "react";

import type { CodeEditorHandle } from "@/components/code-editor";
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
import { Button } from "@/components/ui/button";
import { CardContent, CardDescription, CardFooter, CardHeader } from "@/components/ui/card";
import { FieldError } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toast";
import { APP_DESCRIPTIONS } from "@/content/app";
import { SETTINGS_FILE_DESCRIPTIONS, SETTINGS_FILES } from "@/content/settings-file";
import { capitalized, errorMessage } from "@/lib/utils";
import { settingsFileQueryOptions } from "@/queries/settings-file-queries";

const CodeEditor = lazy(() => import("@/components/code-editor"));

/** Whether a problem found in the browser blocks Save, only where its check is the agent's own parser. */
const BROWSER_CHECK_BLOCKS_SAVE: Record<SettingsFileFormat, boolean> = {
  json: true,
  toml: false,
};

/** An agent's own settings file as text, at the end of its card, saved only when it parses and nothing changed it meanwhile. */
export function SettingsFileSection({ agent }: { agent: Agent }) {
  const ids = { title: useId(), applies: useId() };
  const { data: file, error, isFetching, refetch } = useQuery(settingsFileQueryOptions(agent));
  return (
    <section
      aria-labelledby={ids.title}
      className="flex flex-col gap-(--card-spacing) not-has-data-[slot=card-footer]:pb-(--card-spacing)"
    >
      <CardHeader>
        <h3 id={ids.title} className="text-base font-medium">
          {SETTINGS_FILES[agent].title}
        </h3>
        <CardDescription id={ids.applies}>{SETTINGS_FILES[agent].applies}</CardDescription>
      </CardHeader>
      {file ? (
        <SettingsFileEditor
          agent={agent}
          file={file}
          rereadError={error}
          labelledBy={ids.title}
          describedBy={ids.applies}
        />
      ) : error ? (
        <CardContent className="flex flex-col items-start gap-3">
          <p role="alert" className="text-destructive">
            {errorMessage(error)}
          </p>
          <Button variant="outline" loading={isFetching} onClick={() => void refetch()}>
            {APP_DESCRIPTIONS.retry}
          </Button>
        </CardContent>
      ) : (
        <CardContent className="flex justify-center py-8 text-muted-foreground">
          <Spinner className="size-6" />
        </CardContent>
      )}
    </section>
  );
}

function SettingsFileEditor({
  agent,
  file,
  rereadError,
  labelledBy,
  describedBy,
}: {
  agent: Agent;
  /** The file as the manager last read it. */
  file: SettingsFileText;
  /** Why the manager could not read the file again, such as it no longer being text. */
  rereadError: GetSettingsFileError | null;
  labelledBy: string;
  describedBy: string;
}) {
  const queryClient = useQueryClient();
  const problemId = useId();
  const editor = useRef<CodeEditorHandle>(null);
  const saveButton = useRef<HTMLButtonElement>(null);
  const [opened, setOpened] = useState(file);
  const [loaded, setLoaded] = useState({ text: file.text });
  const [text, setText] = useState(file.text);
  const [marked, setMarked] = useState<ParseProblem>();
  const [judged, setJudged] = useState<{ text: string; problem?: ParseProblem }>();
  const { queryKey } = settingsFileQueryOptions(agent);
  const save = useMutation({
    ...updateSettingsFileMutation(),
    onSuccess: async (saved) => {
      if (document.activeElement === saveButton.current) {
        editor.current?.focus();
      }
      setOpened(saved);
      setJudged({ text: saved.text });
      toast.add({ title: SETTINGS_FILE_DESCRIPTIONS.saved(saved.path) });
      await queryClient.cancelQueries({ queryKey });
      queryClient.setQueryData(queryKey, saved);
    },
    onError: async (error, { body }) => {
      if ("line" in error) {
        setJudged({ text: body.text, problem: error });
        editor.current?.focus();
      } else {
        await queryClient.invalidateQueries({ queryKey });
      }
    },
  });

  const load = (next: SettingsFileText) => {
    setOpened(next);
    setLoaded({ text: next.text });
    setText(next.text);
    setMarked(undefined);
    setJudged(undefined);
  };
  const dirty = text !== opened.text;
  const changedOnDisk = !save.isPending && file.version !== opened.version;
  if (changedOnDisk && text === file.text) {
    setOpened(file);
  } else if (changedOnDisk && !dirty) {
    load(file);
  }
  const verdict = judged?.text === text ? judged : undefined;
  const blocked = verdict
    ? Boolean(verdict.problem)
    : BROWSER_CHECK_BLOCKS_SAVE[file.format] && Boolean(marked);
  const notice = changedOnDisk
    ? SETTINGS_FILE_DESCRIPTIONS.changed_on_disk(file.path)
    : rereadError
      ? errorMessage(rereadError)
      : save.isError && !("line" in save.error)
        ? errorMessage(save.error)
        : undefined;

  return (
    <>
      <CardContent className="flex flex-col gap-2">
        <Suspense
          fallback={
            <div className="flex justify-center py-8 text-muted-foreground">
              <Spinner className="size-6" />
            </div>
          }
        >
          <CodeEditor
            ref={editor}
            format={file.format}
            loaded={loaded}
            serverVerdict={verdict}
            onChange={(next) => {
              setText(next);
              setJudged(undefined);
              if (save.isError) {
                save.reset();
              }
            }}
            onProblem={setMarked}
            aria-labelledby={labelledBy}
            aria-describedby={`${describedBy} ${problemId}`}
          />
        </Suspense>
        {marked ? (
          <FieldError id={problemId}>
            {SETTINGS_FILE_DESCRIPTIONS.problem({ ...marked, error: capitalized(marked.error) })}
          </FieldError>
        ) : null}
      </CardContent>
      <LeaveGuard path={file.path} dirty={dirty} />
      <CardFooter className="flex-wrap justify-between gap-4">
        <p role="alert" className="text-destructive">
          {notice}
        </p>
        <div className="ml-auto flex gap-2">
          <Button
            variant="outline"
            disabled={!dirty || save.isPending}
            onClick={() => {
              save.reset();
              load(file);
              editor.current?.focus();
            }}
          >
            {SETTINGS_FILE_DESCRIPTIONS.revert}
          </Button>
          <Button
            ref={saveButton}
            disabled={!dirty || blocked}
            loading={save.isPending}
            onClick={() => save.mutate({ path: { agent }, body: { text, version: file.version } })}
          >
            {changedOnDisk ? SETTINGS_FILE_DESCRIPTIONS.overwrite : SETTINGS_FILE_DESCRIPTIONS.save}
          </Button>
        </div>
      </CardFooter>
    </>
  );
}

/** Asks before leaving the page while `dirty`. */
function LeaveGuard({ path, dirty }: { path: string; dirty: boolean }) {
  const shouldBlockFn = useCallback(() => dirty, [dirty]);
  const leaving = useBlocker({ shouldBlockFn, enableBeforeUnload: dirty, withResolver: true });
  return (
    <AlertDialog
      open={leaving.status === "blocked"}
      onOpenChange={(open) => {
        if (!open) {
          leaving.reset?.();
        }
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{SETTINGS_FILE_DESCRIPTIONS.leave_title}</AlertDialogTitle>
          <AlertDialogDescription>
            {SETTINGS_FILE_DESCRIPTIONS.leave_description(path)}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{SETTINGS_FILE_DESCRIPTIONS.stay}</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={() => leaving.proceed?.()}>
            {SETTINGS_FILE_DESCRIPTIONS.leave}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
