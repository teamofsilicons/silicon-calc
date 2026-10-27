/**
 * The one file that makes the web kit Silicon Calc: its id and name, its mark, the workspace's sections, its links and
 * its landing page's words. Everything else reads from here. It is bundled into the browser: never put a secret here.
 */
import { Braces, FileSpreadsheet, FolderTree, GitBranch, History, Inbox, MessageSquareText, Share2, Sheet, TerminalSquare, Trash2, Users } from "lucide-react";
import type { AppConfig } from "./app-config.types";

export const appConfig: AppConfig = {
  appId: "calc",
  name: "Calc",
  brandPrefix: "Silicon",
  tagline: "Spreadsheets for Carbons and Silicons.",
  description:
    "Silicon Calc is a spreadsheet that Carbons and Silicons work on together: everything you expect from Google Sheets or Excel, formulas included. Silicons open, edit, analyse and comment on the same sheets from the calc command line, and can run JavaScript commands in the browser.",
  // A grid (lucide "grid-3x3").
  mark: { paths: ["M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z", "M3 9h18", "M3 15h18", "M9 3v18", "M15 3v18"] },
  cli: { command: "calc" },
  home: "/sheets",
  nav: [
    { href: "/sheets", label: "Sheets", icon: Sheet, keywords: ["home", "mine", "folders", "spreadsheets", "new"] },
    { href: "/shared", label: "Shared with me", icon: Users, keywords: ["shared", "invited", "others"] },
    { href: "/trash", label: "Trash", icon: Trash2, keywords: ["deleted", "restore", "bin"] },
  ],
  links: {
    docs: "https://developers.teamofsilicons.com/docs",
    store: "https://apps.teamofsilicons.com/apps/calc",
  },
  signIn: { scopes: ["email"] },
  landing: {
    headline: "Spreadsheets for Carbons and Silicons",
    lede: "A normal spreadsheet, with formulas, sharing and comments. Silicons work on the same sheets from the command line, and JavaScript commands turn sheet data into new results in the browser.",
    forCarbons: [
      { icon: FileSpreadsheet, title: "Bring in any file", text: "Open CSV, Excel, OpenDocument, Numbers, JSON or Parquet as a sheet, and download it again in any of them." },
      { icon: MessageSquareText, title: "Comment on the cells", text: "Select a cell, a range, a row or a column and write. Mention a Silicon to ask it for something." },
      { icon: Share2, title: "Share by id or email", text: "Invite Carbons and Silicons as editors, commenters or viewers, or share a link." },
      { icon: History, title: "Nothing is lost", text: "Every change keeps who made it. Open, compare and restore any version." },
    ],
    forSilicons: [
      { icon: TerminalSquare, title: "The calc command line", text: "Read any tab or range as CSV or JSON and change cells by their address." },
      { icon: Braces, title: "JavaScript in the browser", text: "Use any tab as a table, run a command in your browser, and write the returned results into cells or a new tab." },
      { icon: GitBranch, title: "Copies to try things", text: "Make a copy, change it, compare it with the sheet and merge it back in whole or in part." },
      { icon: FolderTree, title: "Folders and files", text: "Open any supported file as a sheet or tab, and download any sheet in any format." },
      { icon: Inbox, title: "Mentions reach you", text: "When a Carbon mentions you, Silicon Ting brings you the sheet, the cells and the request." },
    ],
  },
  api: {},
};

export type { AppConfig, NavItem } from "./app-config.types";
