import { Autocomplete as AutocompletePrimitive } from "@base-ui/react/autocomplete";
import { cn } from "cn";
import { CheckIcon, ChevronDownIcon } from "lucide-react";
import { useState } from "react";

import { Input } from "@/components/ui/input";

export interface AutocompleteOption {
  value: string;
  description?: string;
}

export interface AutocompleteProps {
  id?: string;
  value: string;
  options: readonly AutocompleteOption[];
  onValueChange: (value: string) => void;
  /** Names the button that opens the list. */
  showOptionsLabel: string;
  placeholder?: string;
  "aria-invalid"?: boolean;
  "aria-describedby"?: string;
}

/**
 * A text input that suggests known values and accepts any other. The list stays closed while
 * nothing matches.
 */
function Autocomplete({
  id,
  value,
  options,
  onValueChange,
  showOptionsLabel,
  placeholder,
  "aria-invalid": invalid,
  "aria-describedby": describedBy,
}: AutocompleteProps) {
  const [open, setOpen] = useState(false);
  const query = value.trim().toLowerCase();
  const exact = options.filter((option) => option.value.toLowerCase() === query);
  const matches =
    !query || exact.length > 0
      ? [...exact, ...options.filter((option) => !exact.includes(option))]
      : options.filter((option) => option.value.toLowerCase().includes(query));
  if (open && matches.length === 0) setOpen(false);
  return (
    <AutocompletePrimitive.Root
      open={open && matches.length > 0}
      onOpenChange={setOpen}
      filteredItems={matches}
      value={value}
      onValueChange={onValueChange}
      itemToStringValue={(option: AutocompleteOption) => option.value}
      openOnInputClick
      autoHighlight
    >
      <div className="relative">
        <AutocompletePrimitive.Input
          id={id}
          placeholder={placeholder}
          aria-invalid={invalid}
          aria-describedby={describedBy}
          spellCheck={false}
          autoComplete="off"
          render={<Input className="pr-8 font-mono" />}
        />
        <AutocompletePrimitive.Trigger
          tabIndex={-1}
          aria-labelledby={undefined}
          aria-label={showOptionsLabel}
          className="absolute inset-y-0 right-0 flex w-8 cursor-pointer items-center justify-center text-muted-foreground outline-none"
        >
          <ChevronDownIcon aria-hidden="true" className="size-4" />
        </AutocompletePrimitive.Trigger>
      </div>
      <AutocompletePrimitive.Portal>
        <AutocompletePrimitive.Positioner sideOffset={4} className="z-50 w-(--anchor-width)">
          <AutocompletePrimitive.Popup className="max-h-72 overflow-y-auto rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10 data-empty:hidden">
            <AutocompletePrimitive.List>
              {matches.map((option, index) => (
                <AutocompletePrimitive.Item
                  key={option.value}
                  value={option}
                  index={index}
                  className="flex cursor-default items-start gap-2 rounded-md px-2 py-1.5 outline-none data-highlighted:bg-accent data-highlighted:text-accent-foreground"
                >
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className="font-mono">{option.value}</span>
                    {option.description ? (
                      <span className="text-muted-foreground">{option.description}</span>
                    ) : null}
                  </span>
                  <CheckIcon
                    aria-hidden="true"
                    className={cn(
                      "mt-0.5 size-4 flex-none",
                      option.value === value.trim() ? "opacity-100" : "opacity-0",
                    )}
                  />
                </AutocompletePrimitive.Item>
              ))}
            </AutocompletePrimitive.List>
          </AutocompletePrimitive.Popup>
        </AutocompletePrimitive.Positioner>
      </AutocompletePrimitive.Portal>
    </AutocompletePrimitive.Root>
  );
}

export { Autocomplete };
