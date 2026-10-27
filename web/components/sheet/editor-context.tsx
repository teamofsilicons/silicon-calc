"use client";

/**
 * What every part of the sheet editor shares: the open line (session and controller, lib/sheet), the sheet and the
 * copy it shows, who is looking and what they may do, which side panel is open, and the comment threads (one list,
 * kept current by the live stream, read by the grid's markers and the comments panel alike).
 */
import { createContext, useContext } from "react";
import type { LineRef } from "@/lib/calc/api";
import type { Account, Copy, Role, Sheet, Thread } from "@/lib/calc/types";
import type { SheetController } from "@/lib/sheet/controller";
import type { SheetSession } from "@/lib/sheet/session";

export type PanelName = "comments" | "history" | "copies" | "code" | "cell";

export interface EditorContextValue {
  session: SheetSession;
  controller: SheetController;
  line: LineRef;
  sheet: Sheet;
  copy: Copy | null;
  me: Account | null;
  role: Role;
  /** May change cells (an editor or the owner, on the current version, on an open copy). */
  canEdit: boolean;
  /** May comment, reply, react and resolve. */
  canComment: boolean;
  isOwner: boolean;
  /** The version open read-only, or null for the current one. */
  atVersion: number | null;
  panel: PanelName | null;
  openPanel: (panel: PanelName | null) => void;
  threads: Thread[];
  /** Puts a thread the stream or an answer brought into the list (created, replied, resolved…). */
  upsertThread: (thread: Thread) => void;
  /** Starts a new comment on the selection (opens the comments panel with the composer). */
  startComment: () => void;
  /** Opens the sharing dialog. */
  openShare: () => void;
  /** Address of a line (main sheet or copy), optionally at a version and tab. */
  hrefOf: (target: { copy: string | null; version?: number | null; tab?: number | null }) => string;
}

export const EditorContext = createContext<EditorContextValue | null>(null);

export function useEditor(): EditorContextValue {
  const value = useContext(EditorContext);
  if (!value) throw new Error("useEditor() is used outside the sheet editor.");
  return value;
}
