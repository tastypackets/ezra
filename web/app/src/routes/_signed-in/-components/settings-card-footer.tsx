import { useRef } from "react";

import { Button } from "@/components/ui/button";
import { CardFooter } from "@/components/ui/card";
import { SETTINGS_DESCRIPTIONS } from "@/content/settings";
import { SETTINGS_FILE_DESCRIPTIONS } from "@/content/settings-file";

import type { SettingsFileDraft } from "./settings-file-section";

export interface SettingsCardFooterProps {
  /** Why the settings could not be saved. */
  error?: string;
  file: SettingsFileDraft;
  submitting: boolean;
  /** Whether any setting differs from what is saved. */
  settingsChanged: boolean;
  onRevert: () => void;
}

/**
 * An agent card's one footer: what went wrong, Revert for every unsaved change, and Save for all of
 * them. Revert hands focus to the editor it reset, else to Save.
 */
export function SettingsCardFooter({
  error,
  file,
  submitting,
  settingsChanged,
  onRevert,
}: SettingsCardFooterProps) {
  const saveButton = useRef<HTMLButtonElement>(null);
  return (
    <CardFooter className="flex-wrap justify-between gap-4">
      <p role="alert" className="text-destructive">
        {error ?? file.notice}
      </p>
      <div className="ml-auto flex gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={!(settingsChanged || file.dirty) || submitting}
          onClick={() => {
            const target = file.dirty ? file.editor.current : saveButton.current;
            onRevert();
            target?.focus();
          }}
        >
          {SETTINGS_FILE_DESCRIPTIONS.revert}
        </Button>
        <Button ref={saveButton} type="submit" disabled={file.blocked} loading={submitting}>
          {file.changedOnDisk && file.dirty
            ? SETTINGS_FILE_DESCRIPTIONS.overwrite
            : SETTINGS_DESCRIPTIONS.save}
        </Button>
      </div>
    </CardFooter>
  );
}
