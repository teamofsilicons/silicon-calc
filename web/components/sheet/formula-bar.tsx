"use client";

/**
 * The formula bar: the address box (shows the selection, takes an address or a range to jump to: `B7`, `A1:C9`,
 * `Sales!A1`) and the formula field, which is the same editor as the cell's (lib/sheet/controller.ts): typing in either
 * shows in both, references are pointed at on the grid, and function names are suggested as they are typed.
 */
import { useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { useControllerRevision, useSessionRevision } from "@/lib/sheet/react";
import { useEditor } from "./editor-context";
import styles from "./editor.module.css";

export function FormulaBar() {
  const { controller, session, canEdit } = useEditor();
  useControllerRevision(controller);
  useSessionRevision(session);
  const input = useRef<HTMLTextAreaElement>(null);
  const [nameDraft, setNameDraft] = useState<string | null>(null);
  const editor = controller.editor;
  const inBar = editor.open && editor.where === "bar";
  const value = editor.open ? editor.text : controller.activeInput();
  const address = controller.selectionText();
  const suggestions = inBar ? controller.suggestions() : [];

  // The controller moves the caret (pointing at a reference, taking a suggestion): follow it here.
  useLayoutEffect(() => {
    const node = input.current;
    if (!node || !inBar || document.activeElement !== node) return;
    if (node.value !== editor.text) node.value = editor.text;
    if (node.selectionStart === node.selectionEnd && node.selectionEnd !== editor.caret) node.setSelectionRange(editor.caret, editor.caret);
  }, [inBar, editor.text, editor.caret]);

  const open = () => {
    if (!canEdit) return;
    if (!controller.editor.open) controller.startEdit({ mode: "edit", where: "bar" });
    else controller.setEditorWhere("bar");
  };

  const sync = () => {
    const node = input.current;
    if (!node || !controller.editor.open) return;
    controller.setEditorText(node.value, node.selectionEnd ?? node.value.length);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!controller.editor.open) return;
    if (suggestions.length) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        controller.moveSuggestion(event.key === "ArrowDown" ? 1 : -1);
        return;
      }
      if (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey && !event.altKey && !event.metaKey && !event.ctrlKey)) {
        event.preventDefault();
        controller.acceptSuggestion();
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        controller.dismissSuggestions();
        return;
      }
    }
    if (event.key === "Enter") {
      event.preventDefault();
      if (event.altKey) {
        controller.insertNewline();
        return;
      }
      controller.commit(event.shiftKey ? "up" : "down", event.metaKey || event.ctrlKey);
      controller.focusGrid?.();
      return;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      controller.commit(event.shiftKey ? "left" : "right");
      controller.focusGrid?.();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      controller.cancelEdit();
      controller.focusGrid?.();
      return;
    }
    if (event.key === "F4") {
      event.preventDefault();
      controller.cycleReference();
    }
  };

  const goTo = (input: string) => {
    const text = input.trim();
    setNameDraft(null);
    if (!text) {
      controller.focusGrid?.();
      return;
    }
    if (!controller.goTo(text)) controller.notice({ kind: "error", title: `“${text}” is not an address`, description: "Type a cell such as B7, a range such as A1:C9, or a tab and a cell such as Sales!A1." });
    controller.focusGrid?.();
  };

  return (
    <div className={styles.formulaBar}>
      <input
        className={styles.nameBox}
        aria-label="Address of the selection; type one to go there"
        value={nameDraft ?? address}
        spellCheck={false}
        autoComplete="off"
        data-testid="name-box"
        onFocus={event => {
          setNameDraft(address);
          const node = event.currentTarget;
          requestAnimationFrame(() => {
            // Navigation may already have returned focus to the grid before this frame.
            if (document.activeElement === node) node.select();
          });
        }}
        onBlur={() => setNameDraft(null)}
        onChange={event => setNameDraft(event.target.value)}
        onKeyDown={event => {
          if (event.key === "Enter") {
            event.preventDefault();
            goTo(event.currentTarget.value);
          } else if (event.key === "Escape") {
            event.preventDefault();
            setNameDraft(null);
            controller.focusGrid?.();
          }
        }}
      />
      <span className={styles.fx} aria-hidden="true">fx</span>
      <textarea
        ref={input}
        className={styles.formulaInput}
        aria-label={`Contents of ${address}`}
        data-testid="formula-input"
        rows={1}
        spellCheck={false}
        autoComplete="off"
        readOnly={!canEdit}
        value={value}
        onFocus={open}
        onPointerDown={open}
        onChange={sync}
        onSelect={sync}
        onKeyDown={onKeyDown}
      />
      {suggestions.length ? (
        <ul className={styles.barSuggestions} role="listbox" aria-label="Functions" data-sq="surface">
          {suggestions.map((item, index) => (
            <li
              key={item.name}
              role="option"
              aria-selected={index === controller.suggestIndex}
              className={styles.barSuggestion}
              onPointerDown={event => {
                event.preventDefault();
                controller.acceptFunction(item.name);
              }}
            >
              <span className={styles.barSuggestionName}>{item.name}</span>
              {item.summary ? <span className={styles.barSuggestionSummary}>{item.summary}</span> : null}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
