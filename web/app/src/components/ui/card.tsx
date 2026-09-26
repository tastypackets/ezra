import { cn } from "cn";

export interface CardProps {
  children: React.ReactNode;
  className?: string;
}

/** A bordered surface panel, the page's main building block. */
export function Card({ children, className }: CardProps) {
  return (
    <section
      className={cn("rounded-lg border border-ez-border bg-ez-surface shadow-ez-card", className)}
    >
      {children}
    </section>
  );
}

export interface CardHeaderProps {
  title: React.ReactNode;
  description?: React.ReactNode;
}

export function CardHeader({ title, description }: CardHeaderProps) {
  return (
    <header className="border-b border-ez-border px-5 py-4">
      <h2 className="text-base font-semibold">{title}</h2>
      {description ? <p className="mt-0.5 text-ez-muted">{description}</p> : null}
    </header>
  );
}

export function CardFooter({ children, className }: CardProps) {
  return (
    <footer
      className={cn(
        "flex items-center justify-between gap-4 rounded-b-lg border-t border-ez-border bg-ez-surface-muted px-5 py-3",
        className,
      )}
    >
      {children}
    </footer>
  );
}
