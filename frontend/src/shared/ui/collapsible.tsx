import {
  DisclosurePanel as CollapsibleContentPrimitive,
  Disclosure as CollapsiblePrimitive,
  DisclosureGroupStateContext,
  Button as CollapsibleTriggerPrimitive,
  type ButtonProps,
  type DisclosurePanelProps,
  type DisclosureProps,
} from "react-aria-components"

function Collapsible({ ...props }: DisclosureProps) {
  // A standalone disclosure must not toggle the enclosing accordion's expanded keys.
  return (
    <DisclosureGroupStateContext.Provider value={null}>
      <CollapsiblePrimitive data-slot="collapsible" {...props} />
    </DisclosureGroupStateContext.Provider>
  )
}

function CollapsibleTrigger({ ...props }: ButtonProps) {
  return (
    <CollapsibleTriggerPrimitive
      slot="trigger"
      data-slot="collapsible-trigger"
      {...props}
    />
  )
}

function CollapsibleContent({ ...props }: DisclosurePanelProps) {
  return (
    <CollapsibleContentPrimitive data-slot="collapsible-content" {...props} />
  )
}

export { Collapsible, CollapsibleTrigger, CollapsibleContent }
