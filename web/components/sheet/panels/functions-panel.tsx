"use client";
import { useEffect, useState } from "react";
import { Button } from "@/components/silicon-ui/button/button";
import { Input } from "@/components/silicon-ui/input/input";
import { Select } from "@/components/silicon-ui/select/select";
import { Textarea } from "@/components/silicon-ui/textarea/textarea";
import { getFunctions, getLibrary, saveFunction, saveLibrary, removeLibrary, type JavascriptFunction, type JavascriptState } from "@/lib/sheet/javascript";
import { runBrowserCommand } from "@/lib/sheet/browser-code";
import { useEditor } from "../editor-context";
import styles from "./panels.module.css";

export function FunctionsPanel() {
  const { session, line, canEdit, me } = useEditor();
  const [state, setState] = useState<JavascriptState | null>(null);
  const [library, setLibrary] = useState<JavascriptFunction[]>([]);
  const [selected, setSelected] = useState<JavascriptFunction | null>(null);
  const [name, setName] = useState("DOUBLE_VALUE"), [parameters, setParameters] = useState("value"), [code, setCode] = useState("return value * 2;");
  const [scope, setScope] = useState("sheet"), [busy, setBusy] = useState(false), [error, setError] = useState(""), [message, setMessage] = useState("");
  useEffect(() => {
    const abort = new AbortController();
    const refresh = () => { void getFunctions(line, abort.signal).then(setState).catch(() => {}); };
    refresh();
    void getLibrary(abort.signal).then(r => setLibrary(r.functions)).catch(() => {});
    let version = session.version;
    const stop = session.subscribe(() => { if (version !== session.version) { version = session.version; refresh(); } });
    return () => { abort.abort(); stop(); };
  }, [line, session]);
  const choose = (f: JavascriptFunction) => { setSelected(f); setName(f.name); setParameters(f.parameters.join(", ")); setCode(f.code); setScope(f.account === me?.uuid ? "account" : "sheet"); setMessage(""); setError(""); };
  const save = async (remove = false) => {
    setBusy(true); setError(""); setMessage("");
    try {
      let f: JavascriptFunction = { name: name.trim().toUpperCase(), parameters: parameters.split(",").map(s => s.trim()).filter(Boolean), code, account: scope === "account" ? me?.uuid : null };
      if (remove && selected?.name === f.name) f = selected;
      if (!remove) {
        if (!/^[A-Z_][A-Z_0-9]{0,63}$/.test(f.name) || f.parameters.some(p => !/^[A-Za-z_][A-Za-z_0-9]{0,63}$/.test(p))) throw new Error("Use letters, numbers and underscores for names, starting with a letter or underscore.");
        await runBrowserCommand(line, `async function ${f.name}(${f.parameters.join(",")}) {\n${code}\n}\nreturn { columns: [], rows: [] };`, { timeoutMs: 5000 });
      }
      if (scope === "account") {
        if (remove) await removeLibrary(f.name); else f = await saveLibrary(f);
        setLibrary((await getLibrary()).functions);
      }
      const current = await getFunctions(line);
      await saveFunction(line, f, current.version, remove);
      setState(await getFunctions(line));
      setMessage(remove ? "Function removed. Copies already used in other sheets stay with those sheets." : `Saved. Use =${f.name}(${f.parameters.map((_,i) => String.fromCharCode(65+i)+"1").join(", ")}) in a cell.`);
    } catch (failure) { setError(failure instanceof Error ? failure.message : "Could not save this function. Try again."); }
    finally { setBusy(false); }
  };
  const functions = [...(state?.functions.filter(f => !f.account || f.account !== me?.uuid) ?? []), ...library];
  return <div className={styles.panel}>
    <p className={styles.secondary}>Give JavaScript a name, then use it in a formula.</p>
    <a href="/docs/javascript#functions" target="_blank" rel="noopener noreferrer">JavaScript guide ↗</a>
    {functions.length ? <Select label="Saved functions" value="new" options={[{ value: "new", label: "Choose a function…" }, ...functions.map((f,i) => ({ value: String(i), label: `${f.name} · ${f.account === me?.uuid ? "My account" : f.account ? "Sheet copy" : "This sheet"}` }))]} onValueChange={value => { const f = functions[Number(value)]; if (f) choose(f); }} /> : null}
    <Input label="Function name" value={name} maxLength={64} onChange={e => setName(e.target.value)} />
    <Input label="Parameters, separated by commas" value={parameters} onChange={e => setParameters(e.target.value)} />
    <Select label="Available in" value={scope} onValueChange={setScope} options={[{ value:"sheet", label:"This sheet" }, { value:"account", label:"All my sheets" }]} />
    <Textarea label="JavaScript function body" value={code} onChange={e => setCode(e.target.value)} rows={10} spellCheck={false} />
    <p className={styles.muted}>Return a value or a rectangular array. Cell arguments are values; ranges are arrays of rows. Sheet definitions take precedence. Account functions used here are saved with this sheet and visible to its readers.</p>
    {error ? <p role="alert">{error}</p> : null}{message ? <p role="status">{message}</p> : null}
    <div className={styles.toolbar}><Button disabled={!canEdit || !name.trim() || !code.trim()} loading={busy} onClick={() => void save()}>Save function</Button><Button variant="ghost" disabled={!canEdit || busy || !state?.functions.some(f => f.name === name.toUpperCase())} onClick={() => void save(true)}>Remove function</Button></div>
    {state?.calls.some(c => c.result?.error) ? <section className={styles.group} aria-label="JavaScript errors"><h3>Calculation errors</h3>{state.calls.filter(c => c.result?.error).map(c => <p key={c.id} className={styles.muted}>{c.name}: {c.result!.error}</p>)}</section> : null}
  </div>;
}
