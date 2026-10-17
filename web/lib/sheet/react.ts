"use client";

/**
 * React's view of one open line: the session (lib/sheet/session.ts) and the editor's controller
 * (lib/sheet/controller.ts) made once per line and version, the live stream opened and closed with the page, and hooks
 * that re-render a component when either changes. The grid itself does not re-render on changes: it subscribes and
 * redraws its canvas (components/sheet/grid).
 */
import { useEffect, useState, useSyncExternalStore } from "react";
import type { Account, CellsAnswer, Sheet, Tab } from "../calc/types";
import type { LineRef } from "../calc/api";
import { SheetController } from "./controller";
import { SheetSession } from "./session";

export interface OpenLineOptions {
  line: LineRef;
  sheet: Sheet;
  tabs: Tab[];
  version: number;
  me: Account | null;
  atVersion: number | null;
  readOnly: boolean;
  /** The tab to open first (from the address), else the first tab. */
  initialTab?: number | null;
  /** The first block of the first tab, read by the server so the grid paints at once. */
  initialCells?: CellsAnswer | null;
}

/** Makes the session and controller for a line once, connects the live stream while mounted. */
export function useOpenLine(options: OpenLineOptions): { session: SheetSession; controller: SheetController } {
  const [state] = useState(() => {
    const session = new SheetSession({ line: options.line, sheet: options.sheet, tabs: options.tabs, version: options.version, me: options.me, atVersion: options.atVersion, readOnly: options.readOnly });
    const first = options.initialTab && options.tabs.some(tab => tab.id === options.initialTab) ? options.initialTab : undefined;
    const controller = new SheetController(session, first);
    if (options.initialCells) session.seed(options.initialCells);
    return { session, controller };
  });
  useEffect(() => {
    state.session.connect();
    return () => state.session.close();
  }, [state]);
  return state;
}

/** Re-renders when the controller changes (selection, editor, tab) and returns its revision. */
export function useControllerRevision(controller: SheetController): number {
  return useSyncExternalStore(
    listener => controller.subscribe(listener),
    () => controller.revision,
    () => controller.revision,
  );
}

/** Re-renders when the session changes (cells, tabs, viewers, live state) and returns its revision. */
export function useSessionRevision(session: SheetSession): number {
  return useSyncExternalStore(
    listener => session.subscribe(listener),
    () => session.revision,
    () => session.revision,
  );
}
