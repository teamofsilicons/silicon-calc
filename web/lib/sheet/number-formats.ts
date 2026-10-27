/**
 * The number formats the toolbar offers (Excel format codes, as the engine reads them: `0.00`, `#,##0`, `0%`,
 * `yyyy-mm-dd`), and the decimal-places buttons' arithmetic on any code.
 */

export interface NumberFormatPreset {
  id: string;
  label: string;
  /** The code sent as `num_fmt` (null: back to Automatic, the engine's General). */
  code: string | null;
  example: string;
}

export const NUMBER_FORMATS: NumberFormatPreset[] = [
  { id: "auto", label: "Automatic", code: null, example: "1000.12" },
  { id: "text", label: "Plain text", code: "@", example: "1000.12" },
  { id: "number", label: "Number", code: "#,##0.00", example: "1,000.12" },
  { id: "integer", label: "Whole number", code: "#,##0", example: "1,000" },
  { id: "percent", label: "Percent", code: "0.00%", example: "10.12%" },
  { id: "scientific", label: "Scientific", code: "0.00E+00", example: "1.01E+03" },
  { id: "currency", label: "Currency", code: "$#,##0.00", example: "$1,000.12" },
  { id: "currency-rounded", label: "Currency (rounded)", code: "$#,##0", example: "$1,000" },
  { id: "accounting", label: "Accounting", code: '_($* #,##0.00_);_($* (#,##0.00);_($* "-"??_);_(@_)', example: "$ (1,000.12)" },
  { id: "financial", label: "Financial", code: '#,##0.00;(#,##0.00);"–"', example: "(1,000.12)" },
  { id: "euro", label: "Euro", code: "€#,##0.00", example: "€1,000.12" },
  { id: "rupee", label: "Indian rupee", code: "₹#,##0.00", example: "₹1,000.12" },
  { id: "fraction", label: "Fraction", code: "# ?/?", example: "1 1/2" },
  { id: "date", label: "Date", code: "yyyy-mm-dd", example: "2026-10-17" },
  { id: "time", label: "Time", code: "hh:mm:ss", example: "14:05:09" },
  { id: "datetime", label: "Date and time", code: "yyyy-mm-dd hh:mm", example: "2026-10-17 14:05" },
  { id: "duration", label: "Duration", code: "[h]:mm:ss", example: "26:15:00" },
];

export const DATE_FORMATS = ["yyyy-mm-dd", "dd/mm/yyyy", "mm/dd/yyyy", "d mmm yyyy", "mmmm d, yyyy", "dddd, mmmm d, yyyy", "yyyy-mm-dd hh:mm:ss", "h:mm AM/PM", "hh:mm:ss", "[h]:mm:ss", "[m]:ss", "[s]"];

export function currencyFormat(currency: string, decimals: number, display: "symbol" | "code", accounting = false): string {
  const parts = new Intl.NumberFormat("en-US", { style: "currency", currency, currencyDisplay: display }).formatToParts(0);
  const symbol = parts.find(part => part.type === "currency")?.value ?? currency;
  const literal = `"${symbol.replace(/"/g, '""')}"${display === "code" ? " " : ""}`;
  const number = `#,##0${decimals > 0 ? `.${"0".repeat(Math.min(10, Math.max(0, decimals)))}` : ""}`;
  return accounting ? `${literal}${number};(${literal}${number});${literal}"–"` : `${literal}${number}`;
}

/** The preset a code is, if any. */
export function presetOf(code: string | null | undefined): NumberFormatPreset | null {
  const clean = code && code.toLowerCase() !== "general" ? code : null;
  return NUMBER_FORMATS.find(preset => preset.code === clean) ?? null;
}

/** True for codes that show dates or times (their digits are not decimal places). */
export function isDateFormat(code: string): boolean {
  return /[dmyhs]/i.test(code.replace(/"[^"]*"|\\.|\[(?![hms]+\])[^\]]*\]/gi, "").replace(/E[+-]/gi, ""));
}

/**
 * Adds (`delta` 1) or removes (`delta` -1) one decimal place in every section of a format code. A cell without a code
 * starts from `0` (or `0.00` when it already shows two places).
 */
export function adjustDecimals(code: string | null | undefined, delta: 1 | -1, shown?: string): string | null {
  let base = code && code.toLowerCase() !== "general" ? code : null;
  if (!base) {
    const places = shown ? (/[.](\d+)/.exec(shown)?.[1]?.length ?? 0) : 0;
    base = places ? `0.${"0".repeat(places)}` : "0";
  }
  if (base === "@" || isDateFormat(base)) return code ?? null;
  return base
    .split(";")
    .map(section => {
      // The last run of digit placeholders before an exponent, a percent or the end is the number's own.
      const match = /([0#?]+)(?:\.([0#?]*))?(?=[^0#?.]*$|E[+-]|%)/i.exec(section);
      if (!match) return section;
      const whole = match[1]!;
      const decimals = match[2] ?? "";
      const nextDecimals = delta > 0 ? `${decimals}0` : decimals.slice(0, -1);
      const replacement = nextDecimals ? `${whole}.${nextDecimals}` : whole;
      return section.slice(0, match.index) + replacement + section.slice(match.index + match[0].length);
    })
    .join(";");
}
