/**
 * What the formula editor says about functions: each one's arguments and one line on what it does, for the
 * suggestions while typing `=SU…` and the hint while the caret is inside `SUM(…)`. Only functions Calc's engine knows
 * are offered (functions.names.ts, kept in step with crates/calc-engine/src/formula/functions.rs by
 * `pnpm sync-functions`); a name the engine knows but this table does not describe is still offered, without words.
 */
import { ENGINE_FUNCTIONS } from "./functions.names";

export interface FunctionInfo {
  name: string;
  /** The arguments, `[optional]` in brackets, `…` for repeats. */
  args: string[];
  summary: string;
}

type Entry = [string, string, string];

/* name, arguments (comma separated), what it does */
const TABLE: Entry[] = [
  // Math and aggregates
  ["SUM", "number1, [number2], …", "Adds numbers and ranges."],
  ["PRODUCT", "number1, [number2], …", "Multiplies numbers and ranges."],
  ["AVERAGE", "number1, [number2], …", "The arithmetic mean of numbers."],
  ["AVERAGEA", "value1, [value2], …", "The mean, counting text as 0 and TRUE as 1."],
  ["COUNT", "value1, [value2], …", "How many values are numbers."],
  ["COUNTA", "value1, [value2], …", "How many values are not empty."],
  ["COUNTBLANK", "range", "How many cells in a range are empty."],
  ["MIN", "number1, [number2], …", "The smallest number."],
  ["MAX", "number1, [number2], …", "The largest number."],
  ["MINA", "value1, [value2], …", "The smallest value, counting text as 0."],
  ["MAXA", "value1, [value2], …", "The largest value, counting text as 0."],
  ["SUMPRODUCT", "array1, [array2], …", "Multiplies matching items of arrays and adds the products."],
  ["SUMSQ", "number1, [number2], …", "The sum of the squares."],
  ["ROUND", "number, digits", "Rounds to a number of digits."],
  ["ROUNDUP", "number, digits", "Rounds away from zero."],
  ["ROUNDDOWN", "number, digits", "Rounds towards zero."],
  ["INT", "number", "Rounds down to the nearest whole number."],
  ["TRUNC", "number, [digits]", "Cuts off the decimals."],
  ["ABS", "number", "The number without its sign."],
  ["SIGN", "number", "1, 0 or -1 by the number's sign."],
  ["MOD", "number, divisor", "The remainder after dividing."],
  ["POWER", "number, power", "A number raised to a power."],
  ["SQRT", "number", "The square root."],
  ["EXP", "number", "e raised to a power."],
  ["LN", "number", "The natural logarithm."],
  ["LOG", "number, [base]", "The logarithm in a base (10 by default)."],
  ["LOG10", "number", "The base-10 logarithm."],
  ["PI", "", "The number π."],
  ["CEILING", "number, significance", "Rounds up to a multiple."],
  ["CEILING.MATH", "number, [significance], [mode]", "Rounds up to a multiple."],
  ["FLOOR", "number, significance", "Rounds down to a multiple."],
  ["FLOOR.MATH", "number, [significance], [mode]", "Rounds down to a multiple."],
  ["MROUND", "number, multiple", "Rounds to the nearest multiple."],
  ["EVEN", "number", "Rounds up to the next even whole number."],
  ["ODD", "number", "Rounds up to the next odd whole number."],
  ["QUOTIENT", "numerator, denominator", "The whole part of a division."],
  ["RAND", "", "A random number from 0 to 1."],
  ["RANDBETWEEN", "bottom, top", "A random whole number between two numbers."],
  ["MEDIAN", "number1, [number2], …", "The middle number."],
  ["MODE", "number1, [number2], …", "The most common number."],
  ["MODE.SNGL", "number1, [number2], …", "The most common number."],
  ["LARGE", "array, k", "The k-th largest value."],
  ["SMALL", "array, k", "The k-th smallest value."],
  ["STDEV", "number1, [number2], …", "The standard deviation of a sample."],
  ["STDEV.S", "number1, [number2], …", "The standard deviation of a sample."],
  ["STDEV.P", "number1, [number2], …", "The standard deviation of a whole population."],
  ["STDEVP", "number1, [number2], …", "The standard deviation of a whole population."],
  ["VAR", "number1, [number2], …", "The variance of a sample."],
  ["VAR.S", "number1, [number2], …", "The variance of a sample."],
  ["VAR.P", "number1, [number2], …", "The variance of a whole population."],
  ["VARP", "number1, [number2], …", "The variance of a whole population."],
  ["RANK", "number, ref, [order]", "Where a number ranks in a list."],
  ["RANK.EQ", "number, ref, [order]", "Where a number ranks in a list."],
  ["SUMIF", "range, criteria, [sum_range]", "Adds the cells that meet a condition."],
  ["SUMIFS", "sum_range, criteria_range1, criteria1, …", "Adds the cells that meet every condition."],
  ["COUNTIF", "range, criteria", "Counts the cells that meet a condition."],
  ["COUNTIFS", "criteria_range1, criteria1, …", "Counts the cells that meet every condition."],
  ["AVERAGEIF", "range, criteria, [average_range]", "The mean of the cells that meet a condition."],
  ["AVERAGEIFS", "average_range, criteria_range1, criteria1, …", "The mean of the cells that meet every condition."],
  ["MAXIFS", "max_range, criteria_range1, criteria1, …", "The largest value that meets every condition."],
  ["MINIFS", "min_range, criteria_range1, criteria1, …", "The smallest value that meets every condition."],
  // Logic
  ["IF", "condition, value_if_true, [value_if_false]", "One value when a condition is true, another when it is not."],
  ["IFS", "condition1, value1, [condition2, value2], …", "The value of the first condition that is true."],
  ["IFERROR", "value, value_if_error", "The value, or another one when it is an error."],
  ["IFNA", "value, value_if_na", "The value, or another one when it is #N/A."],
  ["AND", "logical1, [logical2], …", "TRUE when every argument is true."],
  ["OR", "logical1, [logical2], …", "TRUE when any argument is true."],
  ["XOR", "logical1, [logical2], …", "TRUE when an odd number of arguments are true."],
  ["NOT", "logical", "The opposite of a logical value."],
  ["TRUE", "", "The logical value TRUE."],
  ["FALSE", "", "The logical value FALSE."],
  ["SWITCH", "expression, value1, result1, …, [default]", "The result for the first value that matches."],
  ["CHOOSE", "index, value1, [value2], …", "Picks a value by its position."],
  ["LET", "name1, value1, …, calculation", "Names values to use in a calculation."],
  // Information
  ["ISBLANK", "value", "TRUE when the cell is empty."],
  ["ISNUMBER", "value", "TRUE when the value is a number."],
  ["ISTEXT", "value", "TRUE when the value is text."],
  ["ISNONTEXT", "value", "TRUE when the value is not text."],
  ["ISLOGICAL", "value", "TRUE when the value is TRUE or FALSE."],
  ["ISERROR", "value", "TRUE when the value is any error."],
  ["ISERR", "value", "TRUE when the value is an error other than #N/A."],
  ["ISNA", "value", "TRUE when the value is #N/A."],
  ["ISEVEN", "number", "TRUE when the number is even."],
  ["ISODD", "number", "TRUE when the number is odd."],
  ["ISFORMULA", "reference", "TRUE when the cell holds a formula."],
  ["NA", "", "The #N/A error."],
  ["ERROR.TYPE", "error", "The number of an error kind."],
  ["N", "value", "The value as a number."],
  ["T", "value", "The value when it is text, else empty text."],
  ["TYPE", "value", "The kind of a value as a number."],
  ["FORMULATEXT", "reference", "A cell's formula as text."],
  // Text
  ["CONCAT", "text1, [text2], …", "Joins text."],
  ["CONCATENATE", "text1, [text2], …", "Joins text."],
  ["TEXTJOIN", "delimiter, ignore_empty, text1, …", "Joins text with a delimiter between."],
  ["LEN", "text", "How many characters text has."],
  ["LEFT", "text, [count]", "The first characters of text."],
  ["RIGHT", "text, [count]", "The last characters of text."],
  ["MID", "text, start, count", "Characters from the middle of text."],
  ["UPPER", "text", "Text in capitals."],
  ["LOWER", "text", "Text in lower case."],
  ["PROPER", "text", "Text with each word capitalised."],
  ["TRIM", "text", "Text without extra spaces."],
  ["CLEAN", "text", "Text without characters that do not print."],
  ["TEXT", "value, format", "A value as text in a number format."],
  ["VALUE", "text", "Text that looks like a number, as a number."],
  ["FIND", "find_text, within_text, [start]", "Where text starts in other text (case matters)."],
  ["SEARCH", "find_text, within_text, [start]", "Where text starts in other text (case ignored, wildcards allowed)."],
  ["SUBSTITUTE", "text, old_text, new_text, [instance]", "Replaces text with other text."],
  ["REPLACE", "text, start, count, new_text", "Replaces characters by position."],
  ["REPT", "text, times", "Text repeated."],
  ["EXACT", "text1, text2", "TRUE when two texts are exactly the same."],
  ["CHAR", "number", "The character with a code."],
  ["CODE", "text", "The code of the first character."],
  ["UNICHAR", "number", "The Unicode character with a code."],
  ["UNICODE", "text", "The Unicode code of the first character."],
  ["FIXED", "number, [decimals], [no_commas]", "A number as text with fixed decimals."],
  ["DOLLAR", "number, [decimals]", "A number as currency text."],
  ["NUMBERVALUE", "text, [decimal_separator], [group_separator]", "Text as a number, whatever its separators."],
  ["TEXTBEFORE", "text, delimiter, [instance], …", "The text before a delimiter."],
  ["TEXTAFTER", "text, delimiter, [instance], …", "The text after a delimiter."],
  ["TEXTSPLIT", "text, col_delimiter, [row_delimiter], …", "Splits text into cells."],
  // Lookup and reference
  ["VLOOKUP", "lookup_value, table, column, [approximate]", "Finds a value in the first column and returns one from its row."],
  ["HLOOKUP", "lookup_value, table, row, [approximate]", "Finds a value in the first row and returns one from its column."],
  ["XLOOKUP", "lookup_value, lookup_array, return_array, [if_not_found], [match_mode], [search_mode]", "Finds a value and returns the matching item."],
  ["LOOKUP", "lookup_value, lookup_vector, [result_vector]", "Finds a value in a sorted row or column."],
  ["MATCH", "lookup_value, lookup_array, [match_type]", "Where a value sits in a range."],
  ["XMATCH", "lookup_value, lookup_array, [match_mode], [search_mode]", "Where a value sits in a range."],
  ["INDEX", "array, row, [column]", "The value at a row and column of a range."],
  ["ROW", "[reference]", "The row number of a reference."],
  ["ROWS", "array", "How many rows a range has."],
  ["COLUMN", "[reference]", "The column number of a reference."],
  ["COLUMNS", "array", "How many columns a range has."],
  ["INDIRECT", "ref_text, [a1]", "The reference named by text."],
  ["OFFSET", "reference, rows, cols, [height], [width]", "A range a number of rows and columns away."],
  ["ADDRESS", "row, column, [abs], [a1], [tab]", "A cell address as text."],
  ["SHEET", "[value]", "The number of a tab."],
  ["SHEETS", "[reference]", "How many tabs there are."],
  // Dates
  ["DATE", "year, month, day", "A date from its parts."],
  ["TIME", "hour, minute, second", "A time from its parts."],
  ["TODAY", "", "Today's date."],
  ["NOW", "", "The date and time now."],
  ["YEAR", "date", "The year of a date."],
  ["MONTH", "date", "The month of a date (1 to 12)."],
  ["DAY", "date", "The day of a date (1 to 31)."],
  ["HOUR", "time", "The hour of a time (0 to 23)."],
  ["MINUTE", "time", "The minute of a time."],
  ["SECOND", "time", "The second of a time."],
  ["WEEKDAY", "date, [type]", "The day of the week as a number."],
  ["EDATE", "start_date, months", "The date some months before or after."],
  ["EOMONTH", "start_date, months", "The last day of a month some months away."],
  ["DATEDIF", "start_date, end_date, unit", "The time between two dates in days, months or years."],
  ["DAYS", "end_date, start_date", "The days between two dates."],
  ["DATEVALUE", "date_text", "Text that looks like a date, as a date."],
  ["TIMEVALUE", "time_text", "Text that looks like a time, as a time."],
  ["NETWORKDAYS", "start_date, end_date, [holidays]", "The working days between two dates."],
  ["WORKDAY", "start_date, days, [holidays]", "The date some working days away."],
  ["WEEKNUM", "date, [type]", "The week of the year."],
  ["ISOWEEKNUM", "date", "The ISO week of the year."],
  ["YEARFRAC", "start_date, end_date, [basis]", "The fraction of a year between two dates."],
  // Dynamic arrays
  ["SEQUENCE", "rows, [columns], [start], [step]", "A list of numbers in sequence."],
  ["FILTER", "array, include, [if_empty]", "The rows that meet a condition."],
  ["SORT", "array, [sort_index], [order], [by_col]", "A range sorted."],
  ["SORTBY", "array, by_array1, [order1], …", "A range sorted by other ranges."],
  ["UNIQUE", "array, [by_col], [exactly_once]", "The distinct values of a range."],
  ["TRANSPOSE", "array", "A range turned on its side."],
  ["RANDARRAY", "[rows], [columns], [min], [max], [whole_number]", "An array of random numbers."],
  ["VSTACK", "array1, [array2], …", "Ranges stacked on top of each other."],
  ["HSTACK", "array1, [array2], …", "Ranges placed side by side."],
  ["TAKE", "array, rows, [columns]", "The first or last rows or columns of a range."],
  ["DROP", "array, rows, [columns]", "A range without its first or last rows or columns."],
  ["CHOOSECOLS", "array, col_num1, [col_num2], …", "Chosen columns of a range."],
  ["CHOOSEROWS", "array, row_num1, [row_num2], …", "Chosen rows of a range."],
  ["TOCOL", "array, [ignore], [scan_by_column]", "A range as one column."],
  ["TOROW", "array, [ignore], [scan_by_column]", "A range as one row."],
  // More of Excel, offered as soon as the engine knows them.
  ["COUNTUNIQUE", "value1, [value2], …", "How many distinct values there are."],
  ["IMAGE", "source, [alt_text]", "An image in a cell."],
  ["HYPERLINK", "url, [label]", "A link."],
  ["PMT", "rate, periods, present_value, [future_value], [type]", "The payment of a loan."],
  ["FV", "rate, periods, payment, [present_value], [type]", "The future value of an investment."],
  ["PV", "rate, periods, payment, [future_value], [type]", "The present value of an investment."],
  ["NPV", "rate, value1, [value2], …", "The net present value of cash flows."],
  ["IRR", "values, [guess]", "The internal rate of return."],
  ["CORREL", "array1, array2", "The correlation of two ranges."],
  ["PERCENTILE", "array, k", "The k-th percentile."],
  ["QUARTILE", "array, quart", "A quartile of a range."],
];

const ENGINE = new Set(ENGINE_FUNCTIONS);

const BY_NAME = new Map<string, FunctionInfo>(
  TABLE.map(([name, args, summary]) => [name, { name, args: args ? args.split(/,\s*(?![^[]*\])/).map(part => part.trim()) : [], summary }]),
);

/** What is known about a function the engine has, or null when the engine does not know the name. */
export function functionInfo(name: string): FunctionInfo | null {
  const upper = name.toUpperCase();
  if (!ENGINE.has(upper)) return null;
  return BY_NAME.get(upper) ?? { name: upper, args: [], summary: "" };
}

/** Functions whose name starts with what was typed (then those containing it), at most `limit`. */
export function suggestFunctions(prefix: string, limit = 8): FunctionInfo[] {
  const query = prefix.toUpperCase();
  if (!query) return [];
  const starts: FunctionInfo[] = [];
  const contains: FunctionInfo[] = [];
  for (const name of ENGINE_FUNCTIONS) {
    if (name.startsWith(query)) starts.push(functionInfo(name)!);
    else if (query.length >= 2 && name.includes(query)) contains.push(functionInfo(name)!);
  }
  starts.sort((a, b) => a.name.length - b.name.length || a.name.localeCompare(b.name));
  return [...starts, ...contains].slice(0, limit);
}

/** `SUM(number1, [number2], …)` with the argument at `index` marked for bolding. */
export function signatureParts(info: FunctionInfo, index: number): Array<{ text: string; current: boolean }> {
  const parts: Array<{ text: string; current: boolean }> = [{ text: `${info.name}(`, current: false }];
  const repeating = info.args.findIndex(arg => arg === "…");
  info.args.forEach((arg, i) => {
    if (i > 0) parts.push({ text: ", ", current: false });
    // Past the last named argument of a repeating list, the repeating one is current.
    const current = i === index || (repeating > 0 && index >= repeating && i === repeating - 1);
    parts.push({ text: arg, current });
  });
  parts.push({ text: ")", current: false });
  return parts;
}
