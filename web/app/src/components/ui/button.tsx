import { Button as BaseButton } from "@base-ui/react/button";
import { cn } from "cn";

import { Spinner } from "./spinner";

const VARIANTS = {
  primary: "border-transparent bg-ez-accent text-ez-on-accent hover:bg-ez-accent-hover",
  secondary: "border-ez-border-strong bg-ez-surface text-ez-text-soft hover:bg-ez-surface-muted",
} as const;

const SIZES = {
  md: "h-9 px-4",
  sm: "h-7 px-3 text-[0.8125rem]",
} as const;

/**
 * One `primary` per area, the next step. `secondary` for everything else. `loading` shows a
 * spinner, marks the button busy and blocks clicks while the request runs.
 */
export interface ButtonProps extends Omit<React.ComponentProps<typeof BaseButton>, "className"> {
  variant?: keyof typeof VARIANTS;
  size?: keyof typeof SIZES;
  loading?: boolean;
  className?: string;
}

export function Button({
  variant = "secondary",
  size = "md",
  loading = false,
  className,
  children,
  ...props
}: ButtonProps) {
  return (
    <BaseButton
      aria-busy={loading || undefined}
      className={cn(
        "inline-flex cursor-pointer items-center justify-center gap-2 rounded-md border font-medium whitespace-nowrap shadow-ez-card outline-none tabular-nums transition-colors focus-visible:ring-3 focus-visible:ring-ez-focus disabled:cursor-not-allowed disabled:opacity-60",
        VARIANTS[variant],
        SIZES[size],
        loading && "pointer-events-none cursor-progress",
        className,
      )}
      {...props}
    >
      {loading ? <Spinner /> : null}
      {children}
    </BaseButton>
  );
}
