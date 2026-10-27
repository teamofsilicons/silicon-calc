import type { CellControl, Value } from "../calc/types";
import { valueText } from "./values";

export function checkboxValue(control: Extract<CellControl, { kind: "checkbox" }>, checked: boolean): boolean | string {
  return control.checked === undefined ? checked : checked ? control.checked : control.unchecked ?? "";
}

/** Unknown values remain visible instead of pretending to be unchecked. */
export function checkboxState(control: Extract<CellControl, { kind: "checkbox" }>, value: Value | undefined, pending?: string): boolean | "mixed" {
  if (pending !== undefined) {
    const input = pending.startsWith("'") ? pending.slice(1) : pending;
    if (input === String(checkboxValue(control, true))) return true;
    if (input === String(checkboxValue(control, false))) return false;
    if (control.checked === undefined && /^(TRUE|FALSE)$/i.test(input)) return input.toUpperCase() === "TRUE";
  }
  if (!value || value.t === "empty") return false;
  if (control.checked === undefined) return value.t === "bool" ? value.v : "mixed";
  const text = valueText(value);
  return text === control.checked ? true : text === (control.unchecked ?? "") ? false : "mixed";
}

export function literalInput(value: boolean | string | number): string {
  return typeof value === "number" ? String(value) : typeof value === "boolean" ? value ? "TRUE" : "FALSE" : `'${value}`;
}
