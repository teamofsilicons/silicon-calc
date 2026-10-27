export type ChartValue = number | string | boolean | null;

export interface ChartSeriesData {
  categories: string[];
  series: Array<{ key: string; label: string; values: number[] }>;
}

/** A text first row names the series; a text first column names categories. Keep every numeric series. */
export function chartData(values: ChartValue[][]): ChartSeriesData {
  const rows = values.filter(row => row.some(cell => cell !== null && cell !== ""));
  if (!rows.length) return { categories: [], series: [] };
  const width = rows.reduce((largest, row) => Math.max(largest, row.length), 0);
  const isNumber = (value: ChartValue) => typeof value === "number" || (typeof value === "string" && value.trim() !== "" && Number.isFinite(Number(value.replace(/,/g, ""))));
  const toNumber = (value: ChartValue) => typeof value === "number" ? value : typeof value === "string" ? Number(value.replace(/,/g, "")) || 0 : value ? 1 : 0;
  const header = rows[0]!.slice(1).some(cell => cell !== null && cell !== "" && !isNumber(cell)) || (width === 1 && !isNumber(rows[0]![0] ?? null));
  const body = header ? rows.slice(1) : rows;
  const labelColumn = width > 1 && body.some(row => row[0] !== null && row[0] !== "" && !isNumber(row[0] ?? null));
  const first = labelColumn ? 1 : 0;
  const categories = body.map((row, index) => labelColumn ? String(row[0] ?? "") : String(index + 1));
  const series: ChartSeriesData["series"] = [];
  for (let col = first; col < width; col++) {
    const name = header ? String(rows[0]![col] ?? "") : `Series ${col - first + 1}`;
    series.push({ key: `s${col}`, label: name || `Series ${col - first + 1}`, values: body.map(row => toNumber(row[col] ?? null)) });
  }
  return { categories, series };
}

/** Scatter uses numeric X values, never equally spaced categories. One column uses its row number as X. */
export function scatterData(data: ChartSeriesData) {
  const x = data.series.length > 1 ? data.series[0]! : { label: "Row", values: data.categories.map((_, index) => index + 1) };
  return {
    xLabel: x.label,
    series: (data.series.length > 1 ? data.series.slice(1) : data.series).map(series => ({
      key: series.key,
      label: series.label,
      points: series.values.map((y, index) => ({ x: x.values[index] ?? index + 1, y, label: data.categories[index] ?? String(index + 1) })),
    })),
  };
}

export function chartExtent(values: number[], zero = false): [number, number] {
  let low = zero ? 0 : Infinity, high = zero ? 0 : -Infinity;
  for (const value of values) if (Number.isFinite(value)) { low = Math.min(low, value); high = Math.max(high, value); }
  if (!Number.isFinite(low)) return [0, 1];
  if (low === high) return [low - .5, high + .5];
  return [low, high];
}
