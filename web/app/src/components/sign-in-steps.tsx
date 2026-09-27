import type { LoginPrompt } from "@ezra/client";
import { ExternalLinkIcon } from "lucide-react";

import { buttonVariants } from "@/components/ui/button";
import { CopyButton } from "@/components/ui/copy-button";
import { Spinner } from "@/components/ui/spinner";
import { SIGN_IN_DESCRIPTIONS } from "@/content/sign-in";

export interface SignInStepsProps {
  prompt: LoginPrompt;
  /** Where to paste the code the website shows, for sign-ins without a one-time code. */
  codeForm?: React.ReactNode;
}

/** Numbered steps for a sign-in finished in a browser: open the page, then enter or paste a code. */
export function SignInSteps({ prompt, codeForm }: SignInStepsProps) {
  return (
    <ol className="flex flex-col gap-4">
      <Step number={1} title={SIGN_IN_DESCRIPTIONS.step_open}>
        <a
          href={prompt.url}
          target="_blank"
          rel="noopener noreferrer"
          className={buttonVariants({ variant: "outline", size: "sm" })}
        >
          {siteOf(prompt.url)}
          <ExternalLinkIcon data-icon="inline-end" />
        </a>
      </Step>
      {prompt.code ? (
        <Step number={2} title={SIGN_IN_DESCRIPTIONS.step_enter_code}>
          <CodeToEnter code={prompt.code} />
        </Step>
      ) : (
        <Step number={2} title={SIGN_IN_DESCRIPTIONS.step_paste_code}>
          {codeForm}
        </Step>
      )}
    </ol>
  );
}

/** A code to type on another device, large and monospace, with a button that copies it. */
export function CodeToEnter({ code }: { code: string }) {
  return (
    <div className="flex flex-wrap items-center gap-3">
      <span className="rounded-md border border-dashed px-3 py-1 font-mono text-xl font-semibold tracking-widest break-all">
        {code}
      </span>
      <CopyButton
        text={code}
        label={SIGN_IN_DESCRIPTIONS.copy}
        copiedLabel={SIGN_IN_DESCRIPTIONS.copied}
      />
    </div>
  );
}

/** A spinner and what it waits for. */
export function Waiting({ label }: { label: string }) {
  return (
    <p className="flex items-center gap-2 text-muted-foreground">
      <Spinner />
      {label}
    </p>
  );
}

/** One numbered step: its title, then what to do. */
export function Step({
  number,
  title,
  children,
}: {
  number: number;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <li className="flex gap-3">
      <span className="inline-flex size-6 flex-none items-center justify-center rounded-full bg-muted text-xs font-medium text-muted-foreground">
        {number}
      </span>
      <div className="flex min-w-0 flex-col gap-2">
        <p className="font-medium">{title}</p>
        {children}
      </div>
    </li>
  );
}

/** The host a sign-in link points at, for the button label, e.g. `claude.com`. */
function siteOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
