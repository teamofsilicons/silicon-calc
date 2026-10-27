"use client";

import { useEffect, useMemo, useState } from "react";
import { Dialog, DialogContent } from "@/components/silicon-ui/dialog/dialog";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { Checkbox } from "@/components/silicon-ui/checkbox/checkbox";
import { Button } from "@/components/silicon-ui/button/button";
import { api } from "@/lib/client/api";
import type { CellControl, Value } from "@/lib/calc/types";
import { currencyFormat, DATE_FORMATS } from "@/lib/sheet/number-formats";
import { useEditor } from "./editor-context";
import styles from "./formatting.module.css";

export type FormatDialogKind = "number" | "date" | "currency" | "checkbox" | "dropdown" | "person" | "file" | "text";
const TITLES: Record<FormatDialogKind, string> = { text: "Text appearance", person: "Person chip", file: "File chip", number: "Custom number format", date: "Custom date and time", currency: "Custom currency", checkbox: "Checkbox values", dropdown: "Dropdown options" };

export function FormatDialog({ kind, onClose }: { kind: FormatDialogKind; onClose: () => void }) {
  const { controller } = useEditor();
  const current = controller.activeStyle();
  const control = current.control;
  const [code, setCode] = useState(current.num_fmt ?? (kind === "date" ? "d mmm yyyy" : "#,##0.00"));
  const [currency, setCurrency] = useState("USD");
  const [decimals, setDecimals] = useState("2");
  const [display, setDisplay] = useState<"symbol" | "code">("symbol");
  const [accounting, setAccounting] = useState(false);
  const [custom, setCustom] = useState(control?.kind === "checkbox" && control.checked !== undefined);
  const [checked, setChecked] = useState(control?.kind === "checkbox" ? control.checked ?? "Yes" : "Yes");
  const [unchecked, setUnchecked] = useState(control?.kind === "checkbox" ? control.unchecked ?? "No" : "No");
  const [options, setOptions] = useState(control?.kind === "dropdown" ? control.options.join("\n") : "Not started\nIn progress\nDone");
  const [font, setFont] = useState(current.font ?? "Paper Mono");
  const [size, setSize] = useState(String(current.font_size ?? 10));
  const [angle, setAngle] = useState(String(current.rotation === 255 ? 0 : current.rotation ?? 0));
  const [chipValue, setChipValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [preview, setPreview] = useState<{ text: string; color?: string } | null>(null);
  const currencies = useMemo(() => {
    const names = new Intl.DisplayNames("en", { type: "currency" });
    return Intl.supportedValuesOf("currency").map(value => ({ value, label: `${value} · ${names.of(value) ?? value}` }));
  }, []);
  const format = kind === "currency" ? currencyFormat(currency, Number(decimals), display, accounting) : code;
  const isFormat = kind === "number" || kind === "date" || kind === "currency";
  const active = controller.selection.active;
  const pending = controller.data?.pendingAt(active.row, active.col)?.input;
  const actual = pending !== undefined && pending.trim() && Number.isFinite(Number(pending)) ? { t: "number" as const, v: Number(pending) } : controller.data?.get(active.row, active.col)?.v;
  const sample: Value = actual && actual.t !== "empty" ? actual : { t: "number", v: kind === "date" ? 46320.58691 : 1234.56 };
  const sampleKey = JSON.stringify(sample);
  useEffect(() => {
    if (!isFormat) return;
    const abort = new AbortController();
    const timer = setTimeout(() => {
      void api.post<{ text: string; color?: string }>("/v1/formats/preview", { value: JSON.parse(sampleKey), code: format }, { signal: abort.signal })
        .then(result => { setPreview(result); setError(""); })
        .catch(() => { if (!abort.signal.aborted) { setPreview(null); setError("Could not load the preview. Try again."); } });
    }, 150);
    return () => { clearTimeout(timer); abort.abort(); };
  }, [isFormat, format, sampleKey]);
  const apply = async () => {
    setError("");
    let rule: CellControl | undefined;
    if (kind === "person" || kind === "file") {
      if (kind === "file" && chipValue && !/^(https?:\/\/|\/s\/)/i.test(chipValue)) { setError("Use an http or https file link, or a Calc sheet link."); return; }
      rule = { kind };
    }
    if (kind === "checkbox") {
      if (custom && checked === unchecked) { setError("Use different checked and unchecked values."); return; }
      rule = { kind: "checkbox", ...(custom ? { checked, unchecked } : {}) };
    }
    if (kind === "dropdown") {
      const values = [...new Set(options.split("\n").map(value => value.trim()).filter(Boolean))];
      if (!values.length || values.length > 100 || values.some(value => value.length > 256)) { setError("Add 1 to 100 options, each up to 256 characters."); return; }
      rule = { kind: "dropdown", options: values };
    }
    if (kind === "text" && (!Number.isInteger(Number(size)) || Number(size) < 1 || Number(size) > 400 || !Number.isInteger(Number(angle)) || Number(angle) < -90 || Number(angle) > 90)) {setError("Use a font size from 1 to 400 and an angle from −90 to 90."); return;}
    setBusy(true);
    const ok = kind === "text" ? await controller.send([{type:"style",tab:controller.activeTab,range:controller.apiRange(),patch:{font:font === "Paper Mono" ? null : font.trim(),font_size:Number(size),rotation:Number(angle)}}]) : rule ? await controller.setCellControl(rule) : await controller.send([{ type: "style", tab: controller.activeTab, range: controller.apiRange(), patch: { num_fmt: format || null } }]);
    setBusy(false);
    if (ok && chipValue) controller.setControlValue(active.row, active.col, chipValue);
    if (ok) onClose();
  };
  return <Dialog open onOpenChange={open => { if (!open) onClose(); }}>
    <DialogContent title={TITLES[kind]} description={`Apply to ${controller.apiRange()}`} onCloseAutoFocus={event => { event.preventDefault(); if (!(document.activeElement instanceof HTMLInputElement) && !(document.activeElement instanceof HTMLTextAreaElement)) controller.focusGrid?.(); }}>
      <form className={styles.form} onSubmit={event => { event.preventDefault(); void apply(); }}>
        {kind === "text" ? <><Input label="Font family" value={font} maxLength={100} onChange={e=>setFont(e.target.value)}/><p className={styles.hint}>Paper Mono is included. Other fonts use those available on this device.</p><div className={styles.row}><Input label="Font size" type="number" min={1} max={400} value={size} onChange={e=>setSize(e.target.value)}/><Input label="Rotation (degrees)" type="number" min={-90} max={90} value={angle} onChange={e=>setAngle(e.target.value)}/></div></> : null}
        {kind === "person" || kind === "file" ? <><Input label={kind === "person" ? "Email or account ID (optional)" : "File URL (optional)"} value={chipValue} onChange={e => setChipValue(e.target.value)} /><p className={styles.hint}>Existing cell values become chips. Edit their values in the formula bar.</p></> : null}
        {kind === "currency" ? <>
          <Select label="Currency" value={currency} onValueChange={setCurrency} options={currencies} />
          <div className={styles.row}>
            <Select label="Decimal places" value={decimals} onValueChange={setDecimals} options={Array.from({ length: 11 }, (_, n) => ({ value: String(n), label: String(n) }))} />
            <Select label="Display" value={display} onValueChange={value => setDisplay(value as "symbol" | "code")} options={[{ value: "symbol", label: "Symbol" }, { value: "code", label: "Currency code" }]} />
          </div>
          <Checkbox label="Accounting style" checked={accounting} onCheckedChange={value => setAccounting(value === true)} />
        </> : null}
        {kind === "number" || kind === "date" ? <>
          {kind === "date" ? <Select label="Date and time style" value={DATE_FORMATS.includes(code) ? code : "custom"} onValueChange={value => { if (value !== "custom") setCode(value); }} options={[...DATE_FORMATS.map(value => ({ value, label: value })), { value: "custom", label: "Custom" }]} /> : null}
          <Input label="Format" value={code} maxLength={512} onChange={event => setCode(event.target.value)} placeholder={kind === "date" ? "d mmm yyyy" : '#,##0.00;[Red](#,##0.00);"–"'} />
          {kind === "number" ? <p className={styles.hint}>Separate positive, negative, zero and text formats with semicolons.</p> : null}
        </> : null}
        {isFormat ? <div className={styles.preview} data-sq="surface"><span>Preview</span><output aria-live="polite" style={{ color: preview?.color ?? undefined }}>{preview?.text ?? "…"}</output></div> : null}
        {kind === "checkbox" ? <>
          <Checkbox label="Use custom cell values" checked={custom} onCheckedChange={value => setCustom(value === true)} />
          {custom ? <div className={styles.row}><Input label="Checked" value={checked} maxLength={256} onChange={event => setChecked(event.target.value)} /><Input label="Unchecked" value={unchecked} maxLength={256} onChange={event => setUnchecked(event.target.value)} /></div> : <p className={styles.hint}>Checked cells are TRUE. Unchecked cells are FALSE.</p>}
        </> : null}
        {kind === "dropdown" ? <label className={styles.optionsLabel}>Options, one per line<textarea className={styles.options} data-sq="surface" value={options} onChange={event => setOptions(event.target.value)} rows={6} maxLength={25700} /></label> : null}
        {error ? <p role="alert" className={styles.error}>{error}</p> : null}
        <div className={styles.actions}><Button variant="ghost" onClick={onClose}>Cancel</Button><Button type="submit" loading={busy}>Apply</Button></div>
      </form>
    </DialogContent>
  </Dialog>;
}
