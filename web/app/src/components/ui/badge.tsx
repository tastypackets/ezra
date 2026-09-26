import { cn } from "cn";

const TONES = {
  good: "bg-ez-good-soft text-ez-good",
  pending: "bg-ez-pending-soft text-ez-pending",
  neutral: "bg-ez-neutral-soft text-ez-neutral",
} as const;

export interface BadgeProps {
  tone: keyof typeof TONES;
  children: React.ReactNode;
}

/** A short status label with a soft background in its tone. */
export function Badge({ tone, children }: BadgeProps) {
  return (
    <span className={cn("inline-block rounded-md px-2 py-0.5 text-xs font-medium", TONES[tone])}>
      {children}
    </span>
  );
}
