import { useState } from "react";
import { useTelegramBack } from "@/shared/lib/telegram";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./select";

export interface AppSelectProps {
  id?: string;
  value: string;
  onValueChange: (value: string) => void;
  options: { value: string; label: string; disabled?: boolean }[];
  placeholder?: string;
  disabled?: boolean;
  ariaLabel?: string;
  "aria-label"?: string;
  "aria-describedby"?: string;
  "aria-labelledby"?: string;
  "aria-invalid"?: boolean;
  "data-testid"?: string;
  className?: string;
}

export function AppSelect({ id, value, onValueChange, options, placeholder, disabled, ariaLabel, className, ...props }: AppSelectProps) {
  const [isOpen, setIsOpen] = useState(false);
  useTelegramBack(() => setIsOpen(false), { enabled: isOpen, priority: "overlay" });
  return (
    <Select
      isOpen={isOpen}
      onOpenChange={setIsOpen}
      value={value || null}
      onChange={(key) => { if (key !== null) onValueChange(String(key)); }}
      isDisabled={disabled}
      placeholder={placeholder}
      aria-label={ariaLabel ?? props["aria-label"]}
      aria-labelledby={props["aria-labelledby"]}
      aria-describedby={props["aria-describedby"]}
      isInvalid={props["aria-invalid"]}
      className={className ?? "w-full"}
    >
      <SelectTrigger className="min-h-11" id={id} data-testid={props["data-testid"]}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem id={option.value} key={option.value} textValue={option.label} isDisabled={option.disabled}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}
