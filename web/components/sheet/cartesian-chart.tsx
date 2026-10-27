"use client";

/** Spreadsheet chart kinds that need numeric X coordinates, signed bars or several grouped measures. */
import { useId } from "react";
import type { Chart } from "@/lib/calc/types";
import { chartExtent, scatterData, type ChartSeriesData } from "@/lib/sheet/chart-data";
import styles from "./editor.module.css";

const COLORS = ["var(--accent)", "var(--success)", "var(--warning)", "var(--danger)", "var(--foreground)", "var(--text-secondary)"];
const number = new Intl.NumberFormat("en", { maximumFractionDigits: 3, notation: "compact" });
const short = (label: string) => label.length > 12 ? `${label.slice(0, 11)}…` : label;

export function CartesianChart({ chart, data }: { chart: Chart; data: ChartSeriesData }) {
  const id = useId();
  const scatter = chart.kind === "scatter", horizontal = chart.kind === "bar";
  const points = scatterData(data);
  const series = scatter ? points.series : data.series;
  const width = Math.max(200, chart.width - 24), height = Math.max(100, chart.height - 88);
  const left = horizontal ? 82 : 48, right = width - 14, top = 12, bottom = height - 30;
  const plotWidth = right - left, plotHeight = bottom - top;
  const [low, high] = chartExtent(scatter ? points.series.flatMap(series => series.points.map(point => point.y)) : data.series.flatMap(series => series.values), !scatter);
  const [xLow, xHigh] = scatter ? chartExtent(points.series.flatMap(series => series.points.map(point => point.x))) : [low, high];
  const yOf = (value: number) => bottom - (value - low) / (high - low) * plotHeight;
  const xOf = (value: number) => left + (value - xLow) / (xHigh - xLow) * plotWidth;
  const ticks = Array.from({ length: 5 }, (_, i) => low + (high - low) * i / 4);
  const categoryStep = (horizontal ? plotHeight : plotWidth) / Math.max(1, data.categories.length);
  const barSize = categoryStep * .72 / Math.max(1, data.series.length);
  const labelEvery = Math.max(1, Math.ceil(data.categories.length / Math.max(1, (horizontal ? plotHeight / 22 : plotWidth / 70))));
  return <div className={styles.cartesian}>
    <svg viewBox={`0 0 ${width} ${height}`} role="img" aria-labelledby={`${id}-title ${id}-description`}>
      <title id={`${id}-title`}>{chart.title || `${chart.kind} chart of ${chart.range}`}</title>
      <desc id={`${id}-description`}>{scatter ? `Numeric horizontal axis: ${points.xLabel}.` : "Bars extend from zero, including negative values."} {series.map(item => item.label).join(", ")}. The values are available in the following table.</desc>
      {ticks.map((value, index) => horizontal ? <g key={index}>
        <line x1={xOf(value)} x2={xOf(value)} y1={top} y2={bottom} stroke="var(--border-subtle)" />
        <text x={xOf(value)} y={bottom + 17} textAnchor="middle">{number.format(value)}</text>
      </g> : <g key={index}>
        <line x1={left} x2={right} y1={yOf(value)} y2={yOf(value)} stroke="var(--border-subtle)" />
        <text x={left - 7} y={yOf(value) + 4} textAnchor="end">{number.format(value)}</text>
      </g>)}
      {scatter ? <>
        {Array.from({ length: 5 }, (_, i) => xLow + (xHigh - xLow) * i / 4).map((value, index) => <text key={index} x={xOf(value)} y={bottom + 17} textAnchor="middle">{number.format(value)}</text>)}
        {points.series.map((series, s) => <g key={series.key} fill={COLORS[s % COLORS.length]}>{series.points.map((point, index) => <circle key={index} cx={xOf(point.x)} cy={yOf(point.y)} r="3.5" data-series={series.label} data-x={point.x} data-y={point.y}><title>{series.label}: {points.xLabel} {point.x}, {point.y}</title></circle>)}</g>)}
      </> : <>
        {horizontal ? <line x1={xOf(0)} x2={xOf(0)} y1={top} y2={bottom} stroke="var(--border-strong)" /> : <line x1={left} x2={right} y1={yOf(0)} y2={yOf(0)} stroke="var(--border-strong)" />}
        {data.categories.map((category, index) => index % labelEvery === 0 ? <text key={index} x={horizontal ? left - 7 : left + categoryStep * (index + .5)} y={horizontal ? top + categoryStep * (index + .5) + 4 : bottom + 17} textAnchor={horizontal ? "end" : "middle"}>{short(category)}</text> : null)}
        {data.series.map((series, s) => <g key={series.key} fill={COLORS[s % COLORS.length]}>{series.values.map((value, index) => {
          const start = categoryStep * (index + .14) + barSize * s;
          return <rect key={index} x={horizontal ? Math.min(xOf(0), xOf(value)) : left + start} y={horizontal ? top + start : Math.min(yOf(0), yOf(value))} width={horizontal ? Math.abs(xOf(value) - xOf(0)) : barSize * .9} height={horizontal ? barSize * .9 : Math.abs(yOf(value) - yOf(0))} data-series={series.label} data-value={value}><title>{data.categories[index]} · {series.label}: {value}</title></rect>;
        })}</g>)}
      </>}
    </svg>
    <div className={styles.chartLegend}>{series.map((series, index) => <span key={series.key}><i style={{ background: COLORS[index % COLORS.length] }} aria-hidden="true" />{series.label}</span>)}</div>
    <table className={styles.chartTable}><caption>{chart.title || chart.range}</caption><thead><tr><th scope="col">{scatter ? points.xLabel : "Category"}</th>{series.map(series => <th key={series.key} scope="col">{series.label}</th>)}</tr></thead><tbody>{data.categories.map((category, index) => <tr key={index}><th scope="row">{scatter ? points.series[0]?.points[index]?.x : category}</th>{(scatter ? points.series.map(series => series.points[index]?.y) : data.series.map(series => series.values[index])).map((value, s) => <td key={s}>{value}</td>)}</tr>)}</tbody></table>
  </div>;
}
