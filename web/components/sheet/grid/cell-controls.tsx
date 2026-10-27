"use client";

import { Check, Minus, Star, UserRound, File } from "lucide-react";
import type { SheetController } from "@/lib/sheet/controller";
import { cellName } from "@/lib/sheet/address";
import { checkboxState } from "@/lib/sheet/cell-controls";
import { valueText } from "@/lib/sheet/values";
import { frozenHeight, frozenWidth, visibleRange } from "@/lib/sheet/viewport";
import { useControllerRevision, useSessionRevision } from "@/lib/sheet/react";
import { useViewport } from "./grid";
import type { GridEngine } from "./grid-engine";
import styles from "./grid.module.css";

/** Native, accessible controls over the canvas, limited to visible cells. */
export function CellControls({ engine, controller }: { engine: GridEngine; controller: SheetController }) {
  useViewport(engine);
  useControllerRevision(controller);
  useSessionRevision(controller.session);
  const data = controller.data;
  if (!data) return null;
  const layout = engine.layout();
  const visible = visibleRange(layout, engine.view());
  const areas = [
    { r1: visible.rows.start, r2: visible.rows.end, c1: visible.cols.start, c2: visible.cols.end },
    { r1: 0, r2: visible.frozenRows.end, c1: visible.cols.start, c2: visible.cols.end },
    { r1: visible.rows.start, r2: visible.rows.end, c1: 0, c2: visible.frozenCols.end },
    { r1: 0, r2: visible.frozenRows.end, c1: 0, c2: visible.frozenCols.end },
  ];
  return areas.flatMap(area => {
    if (area.r2 < area.r1 || area.c2 < area.c1) return [];
    return [...data.cellsIn(area)].map(({ row, col, cell }) => {
      const style = cell.s ? controller.session.styles.get(cell.s) : undefined;
      const control = style?.control;
      if (!control || !layout.rows.sizeOf(row) || !layout.cols.sizeOf(col)) return null;
      const merged = data.mergeAt(row, col);
      if (merged && (merged.r1 !== row || merged.c1 !== col)) return null;
      if (controller.editor.open && controller.editor.row === row && controller.editor.col === col) return null;
      const box = engine.cellRect(row, col);
      const left = layout.headerWidth + (col >= layout.frozenCols ? frozenWidth(layout) : 0);
      const top = layout.headerHeight + (row >= layout.frozenRows ? frozenHeight(layout) : 0);
      // Do not paint a scrolling control over a frozen pane or the column/row headings.
      const clipLeft = Math.max(0, left - box.x);
      const clipTop = Math.max(0, top - box.y);
      if (clipLeft >= box.width || clipTop >= box.height) return null;
      const disabled = !controller.canEdit || !!cell.f || !!cell.spill;
      const address = cellName(row, col);
      const selected = controller.selection.active.row === row && controller.selection.active.col === col;
      const select = (extend = false) => {
        if (controller.editor.open) controller.commit("none");
        controller.clickCell(row, col, extend);
      };
      const pending = data.pendingAt(row, col)?.input;
      const value = pending !== undefined ? pending.replace(/^'/, "") : valueText(cell.v);
      return <div key={address} className={styles.cellControl} style={{ left: box.x + 1, top: box.y + 1, width: box.width - 2, height: box.height - 2, clipPath: `inset(${clipTop}px 0 0 ${clipLeft}px)` }}
        onWheel={event => { engine.el.scroller.scrollLeft += event.deltaX; engine.el.scroller.scrollTop += event.deltaY; }}>
        {control.kind === "checkbox" ? <button type="button" role="checkbox" aria-label={`Checkbox ${address}`} aria-checked={checkboxState(control, cell.v, pending)} disabled={disabled}
          tabIndex={selected ? 0 : -1} className={styles.cellCheckbox} data-sq="surface"
          onPointerDown={event => { event.preventDefault(); select(event.shiftKey); }}
          onClick={event => { if (!event.shiftKey) controller.toggleCheckbox(row, col); controller.focusGrid?.(); }}>
          {checkboxState(control, cell.v, pending) === true ? <Check size={14} aria-hidden="true" /> : checkboxState(control, cell.v, pending) === "mixed" ? <Minus size={14} aria-hidden="true" /> : null}
        </button> : control.kind === "dropdown" ? <select aria-label={`Dropdown ${address}`} className={styles.cellDropdown} data-sq="surface" disabled={disabled} tabIndex={selected ? 0 : -1}
          value={value} onPointerDown={() => select()} onFocus={() => { if (!selected) select(); }}
          onChange={event => { controller.setControlValue(row, col, event.target.value); controller.focusGrid?.(); }}>
          {!control.options.includes(value) ? <option value={value} disabled>{value || "Choose…"}</option> : null}
          {control.options.map(option => <option key={option} value={option}>{option}</option>)}
        </select> : control.kind === "rating" ? <div className={styles.cellRating} role="radiogroup" aria-label={`Rating ${address}`}>{[1,2,3,4,5].map(n => <button key={n} type="button" role="radio" aria-label={`${n} ${n === 1 ? "star" : "stars"}`} aria-checked={Number(value) === n} disabled={disabled} tabIndex={selected && (Number(value) === n || (!Number(value) && n === 1)) ? 0 : -1} onPointerDown={e=>{e.preventDefault();select();}} onClick={()=>{controller.setControlValue(row,col,Number(value) === n ? 0 : n);controller.focusGrid?.();}} onKeyDown={e=>{if(e.key === "ArrowRight" || e.key === "ArrowLeft"){e.preventDefault();controller.setControlValue(row,col,Math.max(0,Math.min(5,n+(e.key === "ArrowRight" ? 1 : -1))));}}}><Star size={14} fill={Number(value) >= n ? "currentColor" : "none"} aria-hidden="true" /></button>)}</div> : <div className={styles.cellChip} data-sq="surface" onPointerDown={()=>select()}>{control.kind === "person" ? <UserRound size={13} aria-hidden="true"/> : <File size={13} aria-hidden="true"/>}{control.kind === "file" && /^(https?:\/\/|\/s\/)/i.test(value) ? <a href={value} target="_blank" rel="noopener noreferrer" aria-label={`Open file ${address}: ${value}`} title={value}>{value.split("/").filter(Boolean).pop() || value}</a> : <span title={value}>{value || (control.kind === "person" ? "Person" : "File")}</span>}</div>}

      </div>;
    });
  });
}
