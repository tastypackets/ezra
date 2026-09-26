import { Tooltip as BaseTooltip } from "@base-ui/react/tooltip";

export interface TooltipProps {
  content: React.ReactNode;
  children: React.ReactNode;
}

/** A hint shown on hover or focus of its underlined trigger text. */
export function Tooltip({ content, children }: TooltipProps) {
  return (
    <BaseTooltip.Root>
      <BaseTooltip.Trigger
        render={<span />}
        className="cursor-help underline decoration-ez-border-strong decoration-dotted underline-offset-4"
      >
        {children}
      </BaseTooltip.Trigger>
      <BaseTooltip.Portal>
        <BaseTooltip.Positioner sideOffset={6}>
          <BaseTooltip.Popup className="max-w-64 rounded-md bg-ez-text px-2.5 py-1.5 text-xs text-ez-surface shadow-ez-card">
            {content}
          </BaseTooltip.Popup>
        </BaseTooltip.Positioner>
      </BaseTooltip.Portal>
    </BaseTooltip.Root>
  );
}
