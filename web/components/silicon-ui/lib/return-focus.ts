import { useInsertionEffect, useRef } from "react";

/** What has focus now, unless that is nothing in particular (<body>). */
function focusedElement(): HTMLElement | null {
  const active = typeof document === "undefined" ? null : document.activeElement;
  return active instanceof HTMLElement && active !== document.body ? active : null;
}

export interface ReturnFocusHandlers {
  onOpenAutoFocus: (event: Event) => void;
  onCloseAutoFocus: (event: Event) => void;
}

/**
 * `open`: the layer's open state (null or undefined when the caller cannot know it). `onOpenAutoFocus` /
 * `onCloseAutoFocus`: the caller's own handlers, composed (they run first).
 */
export function useReturnFocus(open: boolean | null | undefined, onOpenAutoFocus?: (event: Event) => void, onCloseAutoFocus?: (event: Event) => void): ReturnFocusHandlers {
  const opener = useRef<HTMLElement | null>(null);
  useInsertionEffect(() => {
    if (open) opener.current = focusedElement();
  }, [open]);
  return {
    onOpenAutoFocus(event) {
      if (open === null || open === undefined) opener.current = focusedElement();
      onOpenAutoFocus?.(event);
    },
    onCloseAutoFocus(event) {
      onCloseAutoFocus?.(event);
      const target = opener.current;
      opener.current = null;
      const layer = event.currentTarget instanceof Element ? event.currentTarget : null;
      if (event.defaultPrevented || !target?.isConnected || layer?.contains(target)) return;
      event.preventDefault();
      const active = document.activeElement;
      if (active && active !== document.body && !layer?.contains(active)) return;
      target.focus();
    },
  };
}
