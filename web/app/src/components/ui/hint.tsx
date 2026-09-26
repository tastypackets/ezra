import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

export interface HintProps {
  /** Plain text, which also names the popup for screen readers. */
  content: string;
  children: React.ReactNode;
}

/**
 * Underlined text with a short explanation. Opens on hover, and on tap or keyboard press, since
 * tooltips never open on touch screens.
 */
function Hint({ content, children }: HintProps) {
  return (
    <Popover>
      <PopoverTrigger
        openOnHover
        className="inline-flex cursor-help underline decoration-muted-foreground/50 decoration-dotted underline-offset-4"
      >
        {children}
      </PopoverTrigger>
      <PopoverContent side="top" aria-label={content} className="w-auto max-w-64 text-xs">
        {content}
      </PopoverContent>
    </Popover>
  );
}

export { Hint };
