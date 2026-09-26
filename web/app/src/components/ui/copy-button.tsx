import { CheckIcon, CopyIcon } from "lucide-react";
import { useState } from "react";

import { Button } from "./button";

const COPIED_LABEL_MS = 1_500;

export interface CopyButtonProps {
  text: string;
  label: string;
  copiedLabel: string;
}

/** Copies `text` to the clipboard. When clipboard access is refused the text stays on screen. */
export function CopyButton({ text, label, copiedLabel }: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), COPIED_LABEL_MS);
    } catch {
      setCopied(false);
    }
  };
  return (
    <Button variant="outline" size="sm" onClick={() => void copy()}>
      {copied ? <CheckIcon data-icon="inline-start" /> : <CopyIcon data-icon="inline-start" />}
      {copied ? copiedLabel : label}
    </Button>
  );
}
