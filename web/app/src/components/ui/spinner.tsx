import { cn } from "cn";

export interface SpinnerProps {
  className?: string;
}

/** A small rotating ring in the current text color. */
export function Spinner({ className }: SpinnerProps) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-block size-3.5 animate-spin rounded-full border-2 border-current border-r-transparent",
        className,
      )}
    />
  );
}
