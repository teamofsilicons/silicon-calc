"use client";

/**
 * Charts over the grid: each chart of the open tab sits at its anchor cell and moves with the cells, drawn with
 * Silicon UI's charts from the values of its range (first row: series names, first column: categories, as a
 * spreadsheet reads a chart's range). Editors drag a chart by its title to move it, by its corner to resize it, and
 * delete it from its menu. Inserting one asks for its kind and title (InsertChartDialog).
 */
import { useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { Ellipsis, Trash2 } from "lucide-react";
import { LineChart } from "@/components/silicon-ui/line-chart/line-chart";
import { DonutChart } from "@/components/silicon-ui/donut-chart/donut-chart";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import { Button } from "@/components/silicon-ui/button/button";
import { HoldToConfirm } from "@/components/silicon-ui/hold-to-confirm/hold-to-confirm";
import { RadioGroup } from "@/components/silicon-ui/radio-group/radio-group";
import { DropdownMenuContent, DropdownMenuItem, DropdownMenuRoot, DropdownMenuTrigger } from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import type { Chart, ChartKind } from "@/lib/calc/types";
import { chartData, type ChartSeriesData } from "@/lib/sheet/chart-data";
import { CartesianChart } from "./cartesian-chart";
import { cellName, parseCell, parseRange } from "@/lib/sheet/address";
import { useSessionRevision } from "@/lib/sheet/react";
import { hitTest } from "@/lib/sheet/viewport";
import type { GridEngine } from "./grid/grid-engine";
import { useViewport } from "./grid/grid";
import { useEditor } from "./editor-context";
import styles from "./editor.module.css";

type Raw = number | string | boolean | null;

function ChartBody({ chart, data }: { chart: Chart; data: ChartSeriesData }) {
  if (!data.series.length || !data.categories.length) return <p className={styles.chartNote}>The range {chart.range} has no numbers to draw yet.</p>;
  const plotHeight = Math.max(80, chart.height - 120);
  if (chart.kind === "pie") {
    const first = data.series[0]!;
    return <DonutChart label={chart.title || first.label} data={data.categories.map((category, index) => ({ key: `${index}:${category}`, label: category, value: Math.max(0, first.values[index] ?? 0) }))} size={Math.min(chart.width - 60, chart.height - 80)} />;
  }
  if (chart.kind === "bar" || chart.kind === "column" || chart.kind === "scatter") return <CartesianChart chart={chart} data={data} />;
  return (
    <LineChart
      label={chart.title || data.series[0]!.label}
      height={plotHeight}
      series={data.series.map((series, index) => ({ key: series.key, label: series.label, area: chart.kind === "area" ? true : index === 0 ? undefined : false }))}
      data={data.categories.map((category, index) => ({ key: `${index}:${category}`, label: category, axisLabel: category, values: Object.fromEntries(data.series.map(series => [series.key, series.values[index] ?? 0])) }))}
      legend={data.series.length > 1}
    />
  );
}

function ChartCard({ engine, chart }: { engine: GridEngine; chart: Chart }) {
  const { controller, session, canEdit } = useEditor();
  useViewport(engine);
  useSessionRevision(session);
  const [moving, setMoving] = useState<{ dx: number; dy: number; width: number; height: number } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const start = useRef<{ x: number; y: number; mode: "move" | "resize" } | null>(null);
  const range = useMemo(() => parseRange(chart.range), [chart.range]);
  // Read again whenever the line moves on (an edit anywhere may change what the range's formulas show).
  const values = useQuery({
    queryKey: ["chart-values", session.line.sheet, session.line.copy, chart.tab, chart.range, session.atVersion, session.version],
    queryFn: () => (range ? session.values(chart.tab, range) : Promise.resolve([] as Raw[][])),
    placeholderData: previous => previous,
    staleTime: 2_000,
  });
  const anchor = parseCell(chart.anchor) ?? { row: 0, col: 0 };
  const box = engine.cellRect(anchor.row, anchor.col);
  const width = moving ? Math.max(240, moving.width) : chart.width;
  const height = moving ? Math.max(180, moving.height) : chart.height;
  const x = box.x + (moving?.dx ?? 0);
  const y = box.y + (moving?.dy ?? 0);
  const bounds = engine.bodyBounds();
  if (x > engine.width || y > engine.height || x + width < bounds.left - 400 || y + height < bounds.top - 400) return null;

  const onPointerDown = (event: ReactPointerEvent<HTMLElement>, mode: "move" | "resize") => {
    if (!canEdit || event.button !== 0) return;
    if ((event.target as HTMLElement).closest("button")) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    start.current = { x: event.clientX, y: event.clientY, mode };
    setMoving({ dx: 0, dy: 0, width: chart.width, height: chart.height });
  };
  const onPointerMove = (event: ReactPointerEvent<HTMLElement>) => {
    const from = start.current;
    if (!from) return;
    const dx = event.clientX - from.x;
    const dy = event.clientY - from.y;
    setMoving(from.mode === "move" ? { dx, dy, width: chart.width, height: chart.height } : { dx: 0, dy: 0, width: chart.width + dx, height: chart.height + dy });
  };
  const onPointerUp = () => {
    const from = start.current;
    const moved = moving;
    start.current = null;
    setMoving(null);
    if (!from || !moved) return;
    if (from.mode === "move") {
      if (Math.abs(moved.dx) < 4 && Math.abs(moved.dy) < 4) return;
      const hit = hitTest(engine.layout(), engine.view(), Math.max(engine.layout().headerWidth + 1, x), Math.max(engine.layout().headerHeight + 1, y));
      void controller.send([{ type: "chart", chart: { id: chart.id, tab: chart.tab, kind: chart.kind, title: chart.title, range: chart.range, anchor: cellName(hit.row, hit.col), width: chart.width, height: chart.height } }]);
    } else {
      void controller.send([{ type: "chart", chart: { id: chart.id, tab: chart.tab, kind: chart.kind, title: chart.title, range: chart.range, anchor: chart.anchor, width: Math.round(Math.max(240, moved.width)), height: Math.round(Math.max(180, moved.height)) } }]);
    }
  };

  return (
    <figure className={styles.chart} data-sq="surface" style={{ transform: `translate(${x}px, ${y}px)`, width, height, margin: 0 }} aria-label={`Chart: ${chart.title || chart.range}`}>
      <figcaption className={styles.chartHead} onPointerDown={event => onPointerDown(event, "move")} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerUp}>
        <span className={styles.chartTitle}>{chart.title || `Chart of ${chart.range}`}</span>
        {canEdit ? (
          <DropdownMenuRoot modal={false}>
            <DropdownMenuTrigger asChild>
              <button type="button" className={styles.iconButton} data-sq="surface" aria-label={`Chart options: ${chart.title || chart.range}`} style={{ width: 30, height: 30 }}>
                <Ellipsis size={16} strokeWidth={1.75} aria-hidden="true" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => controller.focusRange(chart.tab, chart.range)}>Select its range ({chart.range})</DropdownMenuItem>
              <DropdownMenuItem destructive icon={<Trash2 size={16} strokeWidth={1.75} />} onSelect={() => setDeleting(true)}>Delete chart</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenuRoot>
        ) : null}
      </figcaption>
      <div className={styles.chartBody}>
        {values.data ? <ChartBody chart={{ ...chart, width, height }} data={chartData(values.data)} /> : <p className={styles.chartNote}>{values.isError ? "The chart's cells could not be read. It tries again as they change." : "Reading the cells…"}</p>}
      </div>
      {canEdit ? <span className={styles.chartResize} aria-hidden="true" onPointerDown={event => onPointerDown(event, "resize")} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerUp} /> : null}
      <Dialog open={deleting} onOpenChange={setDeleting}><DialogContent title="Delete chart" description="The cells stay in the sheet. You can restore the chart from history."><HoldToConfirm tone="danger" label="Hold to delete chart" onConfirm={() => { void controller.deleteChart(chart.id); setDeleting(false); }} /></DialogContent></Dialog>
    </figure>
  );
}

export function ChartsLayer({ engine }: { engine: GridEngine }) {
  const { controller, session } = useEditor();
  useSessionRevision(session);
  const charts = (controller.data?.charts ?? []).filter(chart => chart.tab === controller.activeTab || chart.tab === 0);
  if (!charts.length) return null;
  return (
    <div className={styles.charts} style={{ clipPath: `inset(${engine.layout().headerHeight}px 0 0 ${engine.layout().headerWidth}px)` }}>
      {charts.map(chart => <ChartCard key={chart.id} engine={engine} chart={chart} />)}
    </div>
  );
}

const KINDS: Array<{ value: ChartKind; label: string; description: string }> = [
  { value: "column", label: "Columns", description: "Compare measures across categories." },
  { value: "bar", label: "Bars", description: "Compare measures with horizontal bars." },
  { value: "line", label: "Line", description: "A trend over time, one line per column." },
  { value: "area", label: "Area", description: "A trend with the area under it filled." },
  { value: "pie", label: "Pie", description: "Parts of one whole." },
  { value: "scatter", label: "Points", description: "Values against each other." },
];

export function InsertChartDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const { controller } = useEditor();
  const [kind, setKind] = useState<ChartKind>("column");
  const [title, setTitle] = useState("");
  const [busy, setBusy] = useState(false);
  const range = controller.apiRange();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent title="Insert a chart" description={`Of ${controller.tab?.name ?? "this tab"}!${range}. ${kind === "scatter" ? "Its first numeric column gives X values; the remaining columns give Y values." : "Its first row names the series and a text first column names the categories."}`}>
        <form
          style={{ display: "grid", gap: "var(--space-4)" }}
          onSubmit={async event => {
            event.preventDefault();
            setBusy(true);
            const ok = await controller.addChart(kind, title.trim());
            setBusy(false);
            if (ok) onOpenChange(false);
          }}
        >
          <RadioGroup label="Kind" options={KINDS} value={kind} onValueChange={value => setKind(value as ChartKind)} />
          <Input label="Title" value={title} placeholder="Revenue by month" onChange={event => setTitle(event.target.value)} />
          <div className={styles.stateActions} style={{ justifyContent: "flex-end" }}>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="submit" loading={busy}>Insert chart</Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
