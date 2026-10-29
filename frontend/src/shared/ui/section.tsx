import { useId, type ReactNode } from "react";
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from "./accordion";
import { Card } from "./card";

export interface SectionProps {
  title: string;
  description?: string;
  icon?: ReactNode;
  children: ReactNode;
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  id?: string;
  "data-testid"?: string;
  className?: string;
}

export function Section({ title, description, icon, children, defaultOpen = false, onOpenChange, id, className, ...props }: SectionProps) {
  const sectionKey = useId();
  return (
    <Card id={id} data-testid={props["data-testid"]} className={className ? `gap-0 py-0 ${className}` : "gap-0 py-0"}>
      <Accordion defaultExpandedKeys={defaultOpen ? [sectionKey] : []} onExpandedChange={(keys) => onOpenChange?.(keys.has(sectionKey))} className="border-0">
        <AccordionItem id={sectionKey}>
          <AccordionTrigger className="items-center p-5">
            <span className="flex min-w-0 items-center gap-3">
              {icon && <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground" aria-hidden="true">{icon}</span>}
              <span className="flex min-w-0 flex-col gap-1">
                <span className="font-medium">{title}</span>
                {description && <span className="text-sm font-normal text-muted-foreground">{description}</span>}
              </span>
            </span>
          </AccordionTrigger>
          <AccordionContent className="px-1 pb-5">{children}</AccordionContent>
        </AccordionItem>
      </Accordion>
    </Card>
  );
}
