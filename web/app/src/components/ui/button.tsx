import { Button as BaseButton } from "@base-ui/react/button";
import { cn } from "cn";

import { Spinner } from "./spinner";

const VARIANTS = {
  primary:
    "border-transparent bg-ez-accent text-ez-on-accent hover:not-data-disabled:bg-ez-accent-hover",
  secondary:
    "border-ez-border-strong bg-ez-surface text-ez-text-soft hover:not-data-disabled:bg-ez-surface-muted",
} as const;

const SIZES = {
  md: "h-9 px-4",
  sm: "h-7 px-3 text-[0.8125rem]",
} as const;

/**
 * One `primary` per area, the next step. `secondary` for everything else. `loading` shows a
 * spinner and disables the button while the request runs, keeping its focus.
 */
export interface ButtonProps extends Omit<React.ComponentProps<typeof BaseButton>, "className"> {
  variant?: keyof typeof VARIANTS;
  size?: keyof typeof SIZES;
  loading?: boolean;
  className?: string;
}

export interface ButtonStyle {
  variant?: keyof typeof VARIANTS;
  size?: keyof typeof SIZES;
}

/** The button look, for a link that should look like a button without acting like one. */
export function buttonClassName({ variant = "secondary", size = "md" }: ButtonStyle): string {
  return cn(
    "inline-flex cursor-pointer items-center justify-center gap-2 rounded-md border font-medium whitespace-nowrap shadow-ez-card outline-none tabular-nums transition-colors focus-visible:ring-3 focus-visible:ring-ez-focus data-disabled:cursor-not-allowed data-disabled:opacity-60",
    VARIANTS[variant],
    SIZES[size],
  );
}

export function Button({
  variant = "secondary",
  size = "md",
  loading = false,
  disabled,
  className,
  children,
  ...props
}: ButtonProps) {
  return (
    <BaseButton
      disabled={loading || disabled}
      focusableWhenDisabled={loading}
      aria-busy={loading || undefined}
      className={cn(
        buttonClassName({ variant, size }),
        loading && "data-disabled:cursor-progress data-disabled:opacity-100",
        className,
      )}
      {...props}
    >
      {loading ? <Spinner /> : null}
      {children}
    </BaseButton>
  );
}
