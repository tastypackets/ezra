import type { Agent, GetSettingsFileError, ParseProblem, SettingsFileText } from "@ezra/client";
import { updateSettingsFileMutation } from "@ezra/client/react-query.gen";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { lazy, Suspense, useId, useRef, useState } from "react";

import type { CodeEditorHandle } from "@/components/code-editor";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { FieldError } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toast";
import { APP_DESCRIPTIONS } from "@/content/app";
import { SETTINGS_FILE_DESCRIPTIONS, SETTINGS_FILES } from "@/content/settings-file";
import { BROWSER_CHECK_BLOCKS_SAVE } from "@/lib/settings-text";
import { capitalized, errorMessage } from "@/lib/utils";
import { settingsFileQueryOptions } from "@/queries/settings-file-queries";

const CodeEditor = lazy(() => import("@/components/code-editor"));

/** An agent's own settings file as text, saved only when it parses and nothing changed it meanwhile. */
export function SettingsFileCard({ agent }: { agent: Agent }) {
  const ids = { title: useId(), applies: useId() };
  const { data: file, error, isFetching, refetch } = useQuery(settingsFileQueryOptions(agent));
  return (
    <Card>
      <CardHeader>
        <CardTitle id={ids.title}>{SETTINGS_FILES[agent].title}</CardTitle>
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
    </Card>
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
  const [edition, setEdition] = useState(0);
  const [editorFocused, setEditorFocused] = useState(false);
  const [focusOnLoad, setFocusOnLoad] = useState(false);
  const [text, setText] = useState(file.text);
  const [checked, setChecked] = useState<ParseProblem>();
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

  const load = (next: SettingsFileText, focus: boolean) => {
    setOpened(next);
    setText(next.text);
    setChecked(undefined);
    setJudged(undefined);
    setFocusOnLoad(focus);
    setEdition(edition + 1);
  };
  const dirty = text !== opened.text;
  const changedOnDisk = !save.isPending && file.version !== opened.version;
  if (changedOnDisk && text === file.text) {
    setOpened(file);
  } else if (changedOnDisk && !dirty) {
    load(file, editorFocused);
  }
  const verdict = judged?.text === text ? judged : undefined;
  const problem = verdict ? verdict.problem : checked;
  const blocked = verdict
    ? Boolean(verdict.problem)
    : BROWSER_CHECK_BLOCKS_SAVE[file.format] && Boolean(checked);
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
        <div onFocus={() => setEditorFocused(true)} onBlur={() => setEditorFocused(false)}>
          <Suspense
            fallback={
              <div className="flex justify-center py-8 text-muted-foreground">
                <Spinner className="size-6" />
              </div>
            }
          >
            <CodeEditor
              key={edition}
              ref={editor}
              format={file.format}
              initialText={opened.text}
              autoFocus={focusOnLoad}
              serverVerdict={verdict}
              onChange={(next) => {
                setText(next);
                setJudged(undefined);
                if (save.isError) {
                  save.reset();
                }
              }}
              onProblem={setChecked}
              aria-labelledby={labelledBy}
              aria-describedby={`${describedBy} ${problemId}`}
            />
          </Suspense>
        </div>
        {problem ? (
          <FieldError id={problemId}>
            {SETTINGS_FILE_DESCRIPTIONS.problem({ ...problem, error: capitalized(problem.error) })}
          </FieldError>
        ) : null}
      </CardContent>
      <CardFooter className="flex-wrap justify-between gap-4">
        <p role="alert" className="text-destructive">
          {notice}
        </p>
        <div className="ml-auto flex gap-2">
          <Button
            variant="outline"
            disabled={!dirty || save.isPending}
            onClick={() => load(file, true)}
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
