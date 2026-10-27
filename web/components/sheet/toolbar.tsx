"use client";

/**
 * The toolbar: undo and redo (through the service, the caller's own changes), number formats and decimal places,
 * text size, bold, italic, underline, strike, text and fill colours, alignment, wrapping, borders, merging, freezing,
 * clearing formats, a chart and a comment on the selection. Every button names its action and shows its state
 * (pressed for the active cell's look). It folds into a row that scrolls on narrow screens.
 */
import { useState, type ReactNode } from "react";
import {
  AlignCenter, AlignLeft, AlignRight, AlignVerticalJustifyCenter, AlignVerticalJustifyEnd, AlignVerticalJustifyStart, Baseline, Bold, ChartColumn,
  ChevronDown, Eraser, Grid2x2, Italic, ListFilter, MessageSquarePlus, PaintBucket, Redo2, RotateCcw, Snowflake, SquareCheck, Strikethrough, TableCellsMerge, Underline, Undo2, WrapText,
} from "lucide-react";
import {
  DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuRoot, DropdownMenuSeparator, DropdownMenuTrigger,
} from "@/components/silicon-ui/dropdown-menu/dropdown-menu";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/silicon-ui/popover/popover";
import { ColorPicker } from "@/components/silicon-ui/color-picker/color-picker";
import { Button } from "@/components/silicon-ui/button/button";
import { Tooltip } from "@/components/silicon-ui/tooltip/tooltip";
import type { BorderStyle, HAlign, VAlign } from "@/lib/calc/types";
import { colName } from "@/lib/sheet/address";
import { adjustDecimals, NUMBER_FORMATS, presetOf } from "@/lib/sheet/number-formats";
import { useControllerRevision, useSessionRevision } from "@/lib/sheet/react";
import { useEditor } from "./editor-context";
import { ConditionalDialog } from "./conditional-dialog";
import { FormatDialog, type FormatDialogKind } from "./format-dialog";
import styles from "./editor.module.css";

const ICON = 16;
const STROKE = 1.75;

export const TEXT_COLORS = [
  "#000000", "#434343", "#666666", "#999999", "#B7B7B7", "#CCCCCC", "#EFEFEF", "#FFFFFF",
  "#B42318", "#C2410C", "#93560A", "#18703F", "#17806B", "#1F5FB8", "#7A4FC4", "#BE185D",
  "#FDECEA", "#FDEFE4", "#FBF1DE", "#E3F2E8", "#E1F3EF", "#E4ECF7", "#EFE9F8", "#FBE7F0",
];

const FONT_SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 24, 36];

function Tool({ label, shortcut, pressed, disabled, onClick, children }: { label: string; shortcut?: string; pressed?: boolean; disabled?: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <Tooltip content={shortcut ? `${label} (${shortcut})` : label}>
      <button
        type="button"
        className={styles.tool}
        data-sq="surface"
        aria-label={label}
        aria-pressed={pressed === undefined ? undefined : pressed}
        aria-keyshortcuts={shortcut}
        disabled={disabled}
        onMouseDown={event => event.preventDefault()}
        onClick={onClick}
      >
        {children}
      </button>
    </Tooltip>
  );
}

function Separator() {
  return <span className={styles.separator} aria-hidden="true" />;
}

/** A colour choice: the palette, a full picker, and "None". */
function ColorTool({ label, icon, value, onChange, resetLabel, disabled }: { label: string; icon: ReactNode; value: string | null; onChange: (color: string | null) => void; resetLabel: string; disabled?: boolean }) {
  const [open, setOpen] = useState(false);
  const [custom, setCustom] = useState(value ?? "#1F5FB8");
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <Tooltip content={label}>
        <PopoverTrigger asChild>
          <button type="button" className={styles.tool} data-sq="surface" aria-label={label} disabled={disabled} onMouseDown={event => event.preventDefault()}>
            <span className={styles.swatchIcon} style={{ ["--swatch" as string]: value ?? "transparent" }}>
              {icon}
              <span className={styles.swatchBar} aria-hidden="true" />
            </span>
          </button>
        </PopoverTrigger>
      </Tooltip>
      <PopoverContent onOpenAutoFocus={event => event.preventDefault()} aria-label={label}>
        <div className={styles.colorPanel}>
          <div className={styles.swatches} role="group" aria-label={label}>
            {TEXT_COLORS.map(color => (
              <button
                key={color}
                type="button"
                className={styles.swatch}
                data-sq="surface"
                style={{ ["--swatch" as string]: color }}
                aria-label={color}
                aria-pressed={value?.toUpperCase() === color}
                onClick={() => {
                  onChange(color);
                  setOpen(false);
                }}
              />
            ))}
          </div>
          <ColorPicker label="Custom" value={custom} onValueChange={hex => setCustom(hex.slice(0, 7))} background="#FFFFFF" />
          <div className={styles.stateActions}>
            <Button size="sm" variant="secondary" onClick={() => { onChange(custom.slice(0, 7).toUpperCase()); setOpen(false); }}>Use this colour</Button>
            <Button size="sm" variant="ghost" className={styles.colorReset} onClick={() => { onChange(null); setOpen(false); }}>{resetLabel}</Button>
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}

export function Toolbar({ onChart }: { onChart: () => void }) {
  const { controller, session, canEdit, canComment, startComment } = useEditor();
  useControllerRevision(controller);
  useSessionRevision(session);
  const [dialog, setDialog] = useState<FormatDialogKind | "conditional" | "alternating" | null>(null);
  const [borderStyle, setBorderStyle] = useState<Exclude<BorderStyle, "none">>("thin");
  const [borderColor, setBorderColor] = useState<string | null>("#000000");
  const border = { style: borderStyle, color: borderColor ?? "#000000" };
  const style = controller.activeStyle();
  const data = controller.data;
  const active = controller.selection.active;
  const shown = data?.get(active.row, active.col)?.t;
  const preset = presetOf(style.num_fmt);
  const formatLabel = style.num_fmt ? (preset ? preset.label : style.num_fmt) : "Automatic";
  const frozenRows = data?.frozenRows ?? 0;
  const frozenCols = data?.frozenCols ?? 0;
  const hAlign: HAlign = style.h_align ?? "general";
  const vAlign: VAlign = style.v_align ?? "bottom";
  const merged = (data?.merges ?? []).some(m => m.r1 <= controller.selection.range.r2 && m.r2 >= controller.selection.range.r1 && m.c1 <= controller.selection.range.c2 && m.c2 >= controller.selection.range.c1);
  const apple = typeof navigator !== "undefined" && /mac|iphone|ipad/i.test(navigator.platform);
  const mod = apple ? "⌘" : "Ctrl+";
  const off = !canEdit;
  const AlignIcon = hAlign === "center" ? AlignCenter : hAlign === "right" ? AlignRight : AlignLeft;

  return (
    <>
    <div className={styles.toolbar} role="toolbar" aria-label="Formatting" aria-orientation="horizontal">
      <Tool label="Undo your last change" shortcut={`${mod}Z`} disabled={off} onClick={() => void controller.session.undo()}>
        <Undo2 size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <Tool label="Redo" shortcut={apple ? "⌘⇧Z" : "Ctrl+Y"} disabled={off} onClick={() => void controller.session.redo()}>
        <Redo2 size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <Separator />

      <DropdownMenuRoot modal={false}>
        <Tooltip content="Number format">
          <DropdownMenuTrigger asChild>
            <button type="button" className={styles.tool} data-sq="surface" aria-label={`Number format ${preset?.id === "percent" ? "%" : preset?.id === "currency" ? "$" : "123"}: ${formatLabel}`} disabled={off} onMouseDown={event => event.preventDefault()}>
              <span className={styles.toolLabel} aria-hidden="true">{preset?.id === "percent" ? "%" : preset?.id === "currency" ? "$" : "123"}</span>
              <ChevronDown size={14} strokeWidth={STROKE} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
        </Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuLabel>Number format</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={preset?.id ?? "custom"} onValueChange={id => controller.setNumberFormat(NUMBER_FORMATS.find(p => p.id === id)?.code ?? null)}>
            {NUMBER_FORMATS.map(item => (
              <DropdownMenuRadioItem key={item.id} value={item.id}>
                {item.label} <span style={{ color: "var(--text-muted)", marginLeft: 8 }}>{item.example}</span>
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => setDialog("currency")}>Custom currency…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("date")}>Custom date and time…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("number")}>Custom number format…</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <Tool label="Insert checkbox" pressed={style.control?.kind === "checkbox"} disabled={off} onClick={() => void controller.setCellControl({ kind: "checkbox" })}>
        <SquareCheck size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <DropdownMenuRoot modal={false}>
        <Tooltip content="Cell controls"><DropdownMenuTrigger asChild><button type="button" className={styles.tool} data-sq="surface" aria-label="Cell controls" disabled={off}><ListFilter size={ICON} strokeWidth={STROKE} aria-hidden="true" /><ChevronDown size={14} aria-hidden="true" /></button></DropdownMenuTrigger></Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuItem onSelect={() => void controller.setCellControl({ kind: "checkbox" })}>Checkbox</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("checkbox")}>Custom checkbox values…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("dropdown")}>Dropdown…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("person")}>Person chip…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("file")}>File chip…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void controller.setCellControl({kind:"rating"})}>Rating</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => void controller.setCellControl(null)}>Remove cell controls</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <Tool label=".0 Fewer decimal places" disabled={off} onClick={() => controller.setNumberFormat(adjustDecimals(style.num_fmt, -1, shown))}>
        <span className={styles.toolLabel} aria-hidden="true">.0</span>
      </Tool>
      <Tool label=".00 More decimal places" disabled={off} onClick={() => controller.setNumberFormat(adjustDecimals(style.num_fmt, 1, shown))}>
        <span className={styles.toolLabel} aria-hidden="true">.00</span>
      </Tool>
      <Separator />

      <DropdownMenuRoot modal={false}>
        <Tooltip content="Font"><DropdownMenuTrigger asChild><button type="button" className={styles.tool} data-sq="surface" aria-label={`Font: ${style.font ?? "Paper Mono"}`} disabled={off}><span className={styles.toolLabel}>{style.font ?? "Paper Mono"}</span><ChevronDown size={14} aria-hidden="true" /></button></DropdownMenuTrigger></Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuRadioGroup value={style.font ?? "Paper Mono"} onValueChange={font => controller.style({ font: font === "Paper Mono" ? null : font })}>
            {["Paper Mono", "Arial", "Helvetica", "Verdana", "Trebuchet MS", "Georgia", "Times New Roman", "Courier New"].map(font => <DropdownMenuRadioItem key={font} value={font}>{font}</DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <DropdownMenuRoot modal={false}>
        <Tooltip content="Text size">
          <DropdownMenuTrigger asChild>
            <button type="button" className={styles.tool} data-sq="surface" aria-label={`Text size: ${style.font_size ?? 10}`} disabled={off} onMouseDown={event => event.preventDefault()}>
              <span className={styles.toolLabel}>{style.font_size ?? 10}</span>
              <ChevronDown size={14} strokeWidth={STROKE} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
        </Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuRadioGroup value={String(style.font_size ?? 10)} onValueChange={size => controller.style({ font_size: Number(size) === 10 ? null : Number(size) })}>
            {FONT_SIZES.map(size => <DropdownMenuRadioItem key={size} value={String(size)}>{size}</DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <Separator />

      <Tool label="Custom text appearance" disabled={off} onClick={() => setDialog("text")}><span aria-hidden="true">Aa</span></Tool>
      <Tool label="Bold" shortcut={`${mod}B`} pressed={!!style.bold} disabled={off} onClick={() => controller.toggle("bold")}>
        <Bold size={ICON} strokeWidth={2} aria-hidden="true" />
      </Tool>
      <Tool label="Italic" shortcut={`${mod}I`} pressed={!!style.italic} disabled={off} onClick={() => controller.toggle("italic")}>
        <Italic size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <Tool label="Underline" shortcut={`${mod}U`} pressed={!!style.underline} disabled={off} onClick={() => controller.toggle("underline")}>
        <Underline size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <Tool label="Strikethrough" pressed={!!style.strike} disabled={off} onClick={() => controller.toggle("strike")}>
        <Strikethrough size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <ColorTool label="Text colour" icon={<Baseline size={ICON} strokeWidth={STROKE} aria-hidden="true" />} value={style.color ?? null} resetLabel="Automatic" disabled={off} onChange={color => controller.style({ color })} />
      <ColorTool label="Fill colour" icon={<PaintBucket size={ICON} strokeWidth={STROKE} aria-hidden="true" />} value={style.fill ?? null} resetLabel="No fill" disabled={off} onChange={fill => controller.style({ fill })} />
      <Separator />

      <DropdownMenuRoot modal={false}>
        <Tooltip content="Align">
          <DropdownMenuTrigger asChild>
            <button type="button" className={styles.tool} data-sq="surface" aria-label="Align" disabled={off} onMouseDown={event => event.preventDefault()}>
              <AlignIcon size={ICON} strokeWidth={STROKE} aria-hidden="true" />
              <ChevronDown size={14} strokeWidth={STROKE} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
        </Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuLabel>Across</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={hAlign} onValueChange={value => controller.style({ h_align: value as HAlign })}>
            <DropdownMenuRadioItem value="general">Automatic</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="left" icon={<AlignLeft size={ICON} strokeWidth={STROKE} />}>Left</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="center" icon={<AlignCenter size={ICON} strokeWidth={STROKE} />}>Centre</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="right" icon={<AlignRight size={ICON} strokeWidth={STROKE} />}>Right</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>Up and down</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={vAlign} onValueChange={value => controller.style({ v_align: value as VAlign })}>
            <DropdownMenuRadioItem value="top" icon={<AlignVerticalJustifyStart size={ICON} strokeWidth={STROKE} />}>Top</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="middle" icon={<AlignVerticalJustifyCenter size={ICON} strokeWidth={STROKE} />}>Middle</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="bottom" icon={<AlignVerticalJustifyEnd size={ICON} strokeWidth={STROKE} />}>Bottom</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <Tool label="Wrap text" pressed={!!style.wrap} disabled={off} onClick={() => controller.style({ wrap: !style.wrap })}>
        <WrapText size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <DropdownMenuRoot modal={false}>
        <Tooltip content="Text layout"><DropdownMenuTrigger asChild><button type="button" className={styles.tool} data-sq="surface" aria-label="Text layout" disabled={off}><RotateCcw size={ICON} strokeWidth={STROKE} aria-hidden="true" /><ChevronDown size={14} aria-hidden="true" /></button></DropdownMenuTrigger></Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuLabel>Wrapping</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={style.wrap ? "wrap" : style.clip ? "clip" : "overflow"} onValueChange={value => controller.style({ wrap: value === "wrap", clip: value === "clip" })}>
            <DropdownMenuRadioItem value="overflow">Overflow</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="wrap">Wrap</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="clip">Clip</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>Rotation</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={String(style.rotation ?? 0)} onValueChange={value => controller.style({ rotation: Number(value) })}>
            {[[0, "None"], [45, "Tilt up"], [-45, "Tilt down"], [255, "Stack vertically"], [90, "Rotate up"], [-90, "Rotate down"]].map(([value, label]) => <DropdownMenuRadioItem key={value} value={String(value)}>{label}</DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <DropdownMenuRoot modal={false}>
        <Tooltip content="Borders">
          <DropdownMenuTrigger asChild>
            <button type="button" className={styles.tool} data-sq="surface" aria-label="Borders" disabled={off} onMouseDown={event => event.preventDefault()}>
              <Grid2x2 size={ICON} strokeWidth={STROKE} aria-hidden="true" />
              <ChevronDown size={14} strokeWidth={STROKE} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
        </Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuLabel>Line style</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={borderStyle} onValueChange={value => setBorderStyle(value as Exclude<BorderStyle, "none">)}>
            {(["thin", "medium", "thick", "dashed", "dotted", "double"] as const).map(value => <DropdownMenuRadioItem key={value} value={value}>{value[0]!.toUpperCase() + value.slice(1)}</DropdownMenuRadioItem>)}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => controller.borders("all", border)}>All borders</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => controller.borders("outer", border)}>Outer borders</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => controller.borders("inner", border)}>Inner borders</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => controller.borders("top", border)}>Top border</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => controller.borders("bottom", border)}>Bottom border</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => controller.borders("left", border)}>Left border</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => controller.borders("right", border)}>Right border</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => controller.borders("bottom", { style: "medium", color: "#000000" })}>Thick bottom border</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => controller.borders("outer", { style: "dashed", color: "#000000" })}>Dashed outer border</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => controller.borders("none")}>No borders</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <ColorTool label="Border colour" icon={<Grid2x2 size={ICON} strokeWidth={STROKE} aria-hidden="true" />} value={borderColor} resetLabel="Automatic" disabled={off} onChange={setBorderColor} />
      <Tool label={merged ? "Unmerge cells" : "Merge cells"} pressed={merged} disabled={off} onClick={() => controller.merge()}>
        <TableCellsMerge size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <DropdownMenuRoot modal={false}><Tooltip content="Merge options"><DropdownMenuTrigger asChild><button type="button" className={styles.tool} data-sq="surface" aria-label="Merge options" disabled={off}><ChevronDown size={14} aria-hidden="true"/></button></DropdownMenuTrigger></Tooltip><DropdownMenuContent style={{ animation: "none" }} align="start"><DropdownMenuItem onSelect={()=>controller.merge()}>Merge all / unmerge</DropdownMenuItem><DropdownMenuItem onSelect={()=>controller.mergeBy("rows")}>Merge horizontally</DropdownMenuItem><DropdownMenuItem onSelect={()=>controller.mergeBy("cols")}>Merge vertically</DropdownMenuItem></DropdownMenuContent></DropdownMenuRoot>
      <DropdownMenuRoot modal={false}>
        <Tooltip content="Freeze rows and columns">
          <DropdownMenuTrigger asChild>
            <button type="button" className={styles.tool} data-sq="surface" aria-label={`Freeze: ${frozenRows} rows, ${frozenCols} columns`} aria-pressed={frozenRows > 0 || frozenCols > 0} disabled={off} onMouseDown={event => event.preventDefault()}>
              <Snowflake size={ICON} strokeWidth={STROKE} aria-hidden="true" />
              <ChevronDown size={14} strokeWidth={STROKE} aria-hidden="true" />
            </button>
          </DropdownMenuTrigger>
        </Tooltip>
        <DropdownMenuContent style={{ animation: "none" }} align="start" onCloseAutoFocus={event => { event.preventDefault(); if (!dialog && !(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
          <DropdownMenuLabel>Rows</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={String(frozenRows)} onValueChange={rows => controller.freeze(Number(rows), null)}>
            <DropdownMenuRadioItem value="0">No rows</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="1">1 row</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="2">2 rows</DropdownMenuRadioItem>
            {active.row + 1 > 2 ? <DropdownMenuRadioItem value={String(active.row + 1)}>Up to row {active.row + 1}</DropdownMenuRadioItem> : null}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>Columns</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={String(frozenCols)} onValueChange={cols => controller.freeze(null, Number(cols))}>
            <DropdownMenuRadioItem value="0">No columns</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="1">1 column</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="2">2 columns</DropdownMenuRadioItem>
            {active.col + 1 > 2 ? <DropdownMenuRadioItem value={String(active.col + 1)}>Up to column {colName(active.col)}</DropdownMenuRadioItem> : null}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenuRoot>
      <Tool label="Conditional formatting" disabled={off} onClick={() => setDialog("conditional")}><span aria-hidden="true">ƒ</span></Tool>
      <Tool label="Alternating colours" disabled={off} onClick={() => setDialog("alternating")}><span aria-hidden="true">▤</span></Tool>
      <Tool label="Clear formatting" shortcut={`${mod}\\`} disabled={off} onClick={() => controller.clearSelection("formats")}>
        <Eraser size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <Separator />
      <Tool label="Insert a chart of the selection" disabled={off} onClick={onChart}>
        <ChartColumn size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
      <Tool label="Comment on the selection" shortcut={apple ? "⌘⌥M" : "Ctrl+Alt+M"} disabled={!canComment} onClick={startComment}>
        <MessageSquarePlus size={ICON} strokeWidth={STROKE} aria-hidden="true" />
      </Tool>
    </div>
    {dialog === "conditional" || dialog === "alternating" ? <ConditionalDialog key={dialog} kind={dialog} onClose={() => setDialog(null)} /> : dialog ? <FormatDialog key={dialog} kind={dialog} onClose={() => setDialog(null)} /> : null}
    </>
  );
}
