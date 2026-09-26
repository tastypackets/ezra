import { cn } from "cn";

interface CellProps {
  children?: React.ReactNode;
  /** Right-aligned with tabular figures, for numbers. */
  numeric?: boolean;
  className?: string;
}

export function Table({ children }: { children: React.ReactNode }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse tabular-nums">{children}</table>
    </div>
  );
}

export function HeaderCell({ children, numeric = false, className }: CellProps) {
  return (
    <th
      className={cn(
        "border-b border-ez-border px-5 py-3 text-left font-semibold whitespace-nowrap text-ez-text-soft",
        numeric && "text-right",
        className,
      )}
    >
      {children}
    </th>
  );
}

export function Cell({ children, numeric = false, className }: CellProps) {
  return (
    <td
      className={cn(
        "border-b border-ez-border px-5 py-3 whitespace-nowrap group-last:border-b-0",
        numeric && "text-right",
        className,
      )}
    >
      {children}
    </td>
  );
}

export function Row({ children }: { children: React.ReactNode }) {
  return <tr className="group">{children}</tr>;
}
