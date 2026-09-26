import type { LoginPrompt } from "@ezra/client";
import { ExternalLink } from "lucide-react";

import { buttonClassName } from "@/components/ui/button";
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
    <ol className="divide-y divide-ez-border px-5">
      <Step number={1} title={SIGN_IN_DESCRIPTIONS.step_open}>
        <a
          href={prompt.url}
          target="_blank"
          rel="noopener noreferrer"
          className={buttonClassName({ size: "sm" })}
        >
          {siteOf(prompt.url)}
          <ExternalLink aria-hidden="true" className="size-3.5" />
        </a>
      </Step>
      {prompt.code ? (
        <Step number={2} title={SIGN_IN_DESCRIPTIONS.step_enter_code}>
          <div className="flex items-center gap-3">
            <span className="rounded-md border border-dashed border-ez-border-strong bg-ez-surface-muted px-3 py-1 font-mono text-xl font-semibold tracking-widest">
              {prompt.code}
            </span>
            <CopyButton
              text={prompt.code}
              label={SIGN_IN_DESCRIPTIONS.copy}
              copiedLabel={SIGN_IN_DESCRIPTIONS.copied}
            />
          </div>
        </Step>
      ) : (
        <Step number={2} title={SIGN_IN_DESCRIPTIONS.step_paste_code}>
          {codeForm}
        </Step>
      )}
    </ol>
  );
}

export function WaitingForWebsite() {
  return (
    <p className="flex items-center gap-2 text-ez-muted">
      <Spinner className="text-ez-accent" />
      {SIGN_IN_DESCRIPTIONS.waiting_for_website}
    </p>
  );
}

function Step({
  number,
  title,
  children,
}: {
  number: number;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <li className="flex gap-3.5 py-3">
      <span className="inline-flex size-6 flex-none items-center justify-center rounded-full bg-ez-neutral-soft text-xs font-semibold text-ez-neutral">
        {number}
      </span>
      <div className="flex flex-col gap-2">
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
