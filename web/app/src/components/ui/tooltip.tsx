import { Popover } from "@base-ui/react/popover";

export interface TooltipProps {
  /** Plain text, which also names the popup for screen readers. */
  content: string;
  children: React.ReactNode;
}

/**
 * A hint on underlined trigger text. Opens on hover, and on tap or keyboard press, since Base UI
 * tooltips never open on touch screens.
 */
export function Tooltip({ content, children }: TooltipProps) {
  return (
    <Popover.Root>
      <Popover.Trigger
        openOnHover
        className="inline-flex cursor-help underline decoration-ez-border-strong decoration-dotted underline-offset-4"
      >
        {children}
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Positioner sideOffset={6}>
          <Popover.Popup
            aria-label={content}
            className="max-w-64 rounded-md bg-ez-text px-2.5 py-1.5 text-xs text-ez-surface shadow-ez-card outline-none"
          >
            {content}
          </Popover.Popup>
        </Popover.Positioner>
      </Popover.Portal>
    </Popover.Root>
  );
}
