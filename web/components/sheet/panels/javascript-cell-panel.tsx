"use client";
import { useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { useEditor } from "../editor-context";
import styles from "./panels.module.css";
export function JavascriptCellPanel() {
  const { controller, canEdit } = useEditor();
  const [code, setCode] = useState("return 2 + 2;");
  return <div className={styles.panel}>
    <p className={styles.secondary}>Write JavaScript for {controller.apiRange().split(":")[0]}. Return a value or an array that spills into nearby cells.</p>
    <a href="/docs/javascript#cells" target="_blank" rel="noopener noreferrer">JavaScript cell guide ↗</a>
    <Textarea label="JavaScript cell body" value={code} onChange={e => setCode(e.target.value)} rows={12} spellCheck={false} />
    <Button disabled={!canEdit || !code.trim()} onClick={() => { void controller.send([{type:"set",tab:controller.activeTab,cells:[{a:controller.apiRange().split(":")[0]!,input:`=JS("${code.replace(/"/g, '""')}")`}]}]); }}>Apply to cell</Button>
  </div>;
}
