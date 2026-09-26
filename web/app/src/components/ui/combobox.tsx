import { Autocomplete } from "@base-ui/react/autocomplete";
import { cn } from "cn";
import { Check, ChevronDown } from "lucide-react";

import { TEXT_INPUT_CLASS } from "./field";

export interface ComboboxOption {
  value: string;
  description?: string;
}

export interface ComboboxProps {
  value: string;
  options: readonly ComboboxOption[];
  onValueChange: (value: string) => void;
  /** Names the button that opens the list. */
  showOptionsLabel: string;
}

/**
 * A text input that suggests known values and takes any other, for settings a CLI may learn
 * before the manager does. Place it in a `Field` for its label.
 */
export function Combobox({ value, options, onValueChange, showOptionsLabel }: ComboboxProps) {
  const query = value.trim().toLowerCase();
  const exact = options.filter((option) => option.value.toLowerCase() === query);
  const matches =
    !query || exact.length > 0
      ? [...exact, ...options.filter((option) => !exact.includes(option))]
      : options.filter((option) => option.value.toLowerCase().includes(query));
  return (
    <Autocomplete.Root
      filteredItems={matches}
      value={value}
      onValueChange={onValueChange}
      itemToStringValue={(option: ComboboxOption) => option.value}
      openOnInputClick
      autoHighlight
    >
      <div className="relative">
        <Autocomplete.Input
          spellCheck={false}
          autoComplete="off"
          className={cn(TEXT_INPUT_CLASS, "pr-9 font-mono")}
        />
        <Autocomplete.Trigger
          tabIndex={-1}
          aria-labelledby={undefined}
          aria-label={showOptionsLabel}
          className="absolute inset-y-0 right-0 flex w-9 cursor-pointer items-center justify-center text-ez-muted outline-none"
        >
          <ChevronDown aria-hidden="true" className="size-4" />
        </Autocomplete.Trigger>
      </div>
      <Autocomplete.Portal>
        <Autocomplete.Positioner sideOffset={4} className="z-50 w-(--anchor-width)">
          <Autocomplete.Popup className="max-h-72 overflow-y-auto rounded-md border border-ez-border bg-ez-surface p-1 shadow-ez-card data-empty:hidden">
            <Autocomplete.List>
              {matches.map((option, index) => (
                <Autocomplete.Item
                  key={option.value}
                  value={option}
                  index={index}
                  className="flex cursor-pointer items-start gap-2 rounded-sm px-2.5 py-1.5 data-highlighted:bg-ez-surface-muted"
                >
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className="font-mono">{option.value}</span>
                    {option.description ? (
                      <span className="text-ez-muted">{option.description}</span>
                    ) : null}
                  </span>
                  {option.value === value.trim() ? (
                    <Check aria-hidden="true" className="mt-0.5 size-4 flex-none text-ez-accent" />
                  ) : null}
                </Autocomplete.Item>
              ))}
            </Autocomplete.List>
          </Autocomplete.Popup>
        </Autocomplete.Positioner>
      </Autocomplete.Portal>
    </Autocomplete.Root>
  );
}
