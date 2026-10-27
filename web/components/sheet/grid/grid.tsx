"use client";

/**
 * The grid: the canvas, the native scroller over it, and the one textarea the keyboard lives in (grid-engine.ts does
 * the work). React renders what floats over it: function suggestions and the signature hint while typing a formula,
 * the comment preview when the pointer rests on a commented cell, and the charts (charts-layer.tsx).
 */
import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import type { Thread } from "@/lib/calc/types";
import { cellKey, formatRange } from "@/lib/sheet/address";
import type { SheetController } from "@/lib/sheet/controller";
import { functionAt } from "@/lib/sheet/formula";
import { functionInfo, signatureParts } from "@/lib/sheet/functions";
import { useControllerRevision } from "@/lib/sheet/react";
import { formatRelative } from "@/lib/format";
import { GridEngine, type GridCallbacks } from "./grid-engine";
import { CellControls } from "./cell-controls";
import styles from "./grid.module.css";

export interface GridProps {
  controller: SheetController;
  /** Open threads by the cell they start on (cell key), on the active tab. */
  threadsByCell: Map<number, Thread>;
  onContextMenu: GridCallbacks["onContextMenu"];
  onComment: () => void;
  onOpenThread: (thread: Thread) => void;
  onDropFiles: (files: File[]) => void;
  /** Hands the engine to the editor (charts, panels that scroll the grid). */
  onEngine?: (engine: GridEngine | null) => void;
  /** Floats over the cells, positioned by the engine (the charts). */
  overlay?: (engine: GridEngine) => ReactNode;
}

export function Grid({ controller, threadsByCell, onContextMenu, onComment, onOpenThread, onDropFiles, onEngine, overlay }: GridProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const spacerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const mirrorRef = useRef<HTMLDivElement>(null);
  const [engine, setEngine] = useState<GridEngine | null>(null);
  const [hover, setHover] = useState<{ row: number; col: number; x: number; y: number; width: number; height: number } | null>(null);
  const callbacks = useRef<GridCallbacks>({ onContextMenu, onHover: setHover, onComment, onDropFiles });
  useLayoutEffect(() => {
    callbacks.current = { onContextMenu, onHover: setHover, onComment, onDropFiles };
  }, [onContextMenu, onComment, onDropFiles]);

  useLayoutEffect(() => {
    const el = { root: rootRef.current!, canvas: canvasRef.current!, scroller: scrollerRef.current!, spacer: spacerRef.current!, input: inputRef.current!, mirror: mirrorRef.current! };
    const created = new GridEngine(el, controller, {
      onContextMenu: (x, y, area) => callbacks.current.onContextMenu(x, y, area),
      onHover: hovered => callbacks.current.onHover(hovered),
      onComment: () => callbacks.current.onComment(),
      onDropFiles: files => callbacks.current.onDropFiles(files),
    });
    setEngine(created);
    onEngine?.(created);
    created.focus();
    return () => {
      onEngine?.(null);
      created.destroy();
    };
    // The engine lives as long as the controller (one per open line).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [controller]);

  useEffect(() => {
    engine?.setCommentCells(new Set(threadsByCell.keys()));
  }, [engine, threadsByCell]);

  useControllerRevision(controller);
  const data = controller.data;
  const active = controller.selection.active;
  const shown = data?.get(active.row, active.col)?.t ?? "";
  const address = formatRange(controller.selection.range, data?.rows.count, data?.cols.count);
  const label = controller.editor.open ? `Editing ${address}` : `Cell ${address}${shown ? `, ${shown}` : ", empty"}`;
  const hoverThread = hover ? threadsByCell.get(cellKey(hover.row, hover.col)) ?? null : null;

  return (
    <div ref={rootRef} className={styles.root} data-testid="grid">
      <canvas ref={canvasRef} className={styles.canvas} aria-hidden="true" />
      <div ref={scrollerRef} className={styles.scroller} data-testid="grid-scroller">
        <div ref={spacerRef} className={styles.spacer} />
      </div>
      {engine && overlay ? overlay(engine) : null}
      {engine ? <CellControls engine={engine} controller={controller} /> : null}
      <div ref={mirrorRef} className={styles.mirror} aria-hidden="true" />
      <textarea
        ref={inputRef}
        className={styles.input}
        data-editing="false"
        data-testid="cell-input"
        aria-label={label}
        aria-roledescription="spreadsheet"
        spellCheck={false}
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="off"
        rows={1}
        defaultValue=""
      />
      <p className="sr-only" aria-live="polite">{controller.editor.open ? "" : label}</p>
      {engine ? <EditorHints engine={engine} controller={controller} /> : null}
      {hover && hoverThread ? <CommentPreview thread={hoverThread} box={hover} onOpen={() => onOpenThread(hoverThread)} /> : null}
    </div>
  );
}

/** Follows the engine's viewport (scrolling, resizing) with a re-render. */
export function useViewport(engine: GridEngine): number {
  return useSyncExternalStore(
    listener => engine.subscribeView(listener),
    () => engine.viewRevision,
    () => engine.viewRevision,
  );
}

/** Function suggestions while a name is typed, then the function's arguments while the caret is inside it. */
function EditorHints({ engine, controller }: { engine: GridEngine; controller: SheetController }) {
  useViewport(engine);
  useControllerRevision(controller);
  const editor = controller.editor;
  if (!editor.open || editor.where !== "cell" || editor.tab !== controller.activeTab) return null;
  const box = engine.cellRect(editor.row, editor.col);
  const top = Math.min(engine.height - 40, box.y + Math.max(box.height, editorHeight(engine)) + 4);
  const left = Math.max(engine.layout().headerWidth, Math.min(box.x, engine.width - 320));
  const items = controller.suggestions();
  if (items.length) {
    return (
      <ul className={styles.suggestions} role="listbox" aria-label="Functions" style={{ transform: `translate(${left}px, ${top}px)` }} data-sq="surface">
        {items.map((item, index) => (
          <li
            key={item.name}
            role="option"
            aria-selected={index === controller.suggestIndex}
            className={styles.suggestion}
            onPointerDown={event => {
              event.preventDefault();
              controller.acceptFunction(item.name);
            }}
          >
            <span className={styles.suggestionName}>{item.name}</span>
            {item.summary ? <span className={styles.suggestionSummary}>{item.summary}</span> : null}
          </li>
        ))}
      </ul>
    );
  }
  const inside = functionAt(editor.text, editor.caret);
  const info = inside ? functionInfo(inside.name) : null;
  if (!inside || !info || !info.args.length) return null;
  return (
    <div className={styles.signature} style={{ transform: `translate(${left}px, ${top}px)` }} data-sq="surface" role="status">
      <code className={styles.signatureCode}>
        {signatureParts(info, inside.argument).map((part, index) => (part.current ? <strong key={index}>{part.text}</strong> : <span key={index}>{part.text}</span>))}
      </code>
      {info.summary ? <span className={styles.signatureSummary}>{info.summary}</span> : null}
    </div>
  );
}

function editorHeight(engine: GridEngine): number {
  const height = engine.el.mirror.style.height;
  return height ? Number.parseFloat(height) : 0;
}

/** The first comment of a thread, shown while the pointer rests on its cell. */
function CommentPreview({ thread, box, onOpen }: { thread: Thread; box: { x: number; y: number; width: number; height: number }; onOpen: () => void }) {
  const first = thread.comments[0];
  if (!first) return null;
  const replies = thread.comments.length - 1;
  return (
    <button type="button" className={styles.preview} style={{ transform: `translate(${box.x + box.width + 6}px, ${box.y}px)` }} data-sq="surface" onClick={onOpen}>
      <span className={styles.previewMeta}>
        <span className={styles.previewAuthor}>{first.author.id}</span>
        <span className={styles.previewTime}>{formatRelative(first.created_at)}</span>
      </span>
      <span className={styles.previewBody}>{first.body}</span>
      {replies > 0 ? <span className={styles.previewReplies}>{replies === 1 ? "1 reply" : `${replies} replies`}</span> : null}
    </button>
  );
}
