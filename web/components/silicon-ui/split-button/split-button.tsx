"use client";

import { isValidElement } from "react";
import type { CSSProperties, ReactNode } from "react";
import * as DropdownPrimitive from "@radix-ui/react-dropdown-menu";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import type { TargetAndTransition } from "motion/react";
import { ChevronDown } from "lucide-react";
import { MenuHighlight, useMenuHighlight } from "../lib/menu-highlight";
import { motionTokens } from "../lib/motion-tokens";
import { TextMorph } from "../text-morph/text-morph";
import styles from "./split-button.module.css";

export interface SplitButtonAction { label: string; onSelect?: () => void; disabled?: boolean; destructive?: boolean; icon?: ReactNode; }
export interface SplitButtonProps { label: string; actions: SplitButtonAction[]; onClick?: () => void; disabled?: boolean; icon?: ReactNode; variant?: "primary" | "secondary"; }

const rest: TargetAndTransition = { opacity: 1, y: 0, scale: 1, filter: "blur(0px)" };
const fadeIn: TargetAndTransition = { ...rest, opacity: 0 };
const fadeOut: TargetAndTransition = { opacity: 0, transition: { duration: motionTokens.duration.instant } };
const iconIn: TargetAndTransition = { opacity: 0, scale: .6, filter: `blur(${motionTokens.blur.subtle}px)` };
const iconOut: TargetAndTransition = { ...iconIn, transition: { duration: motionTokens.duration.fast, ease: [...motionTokens.ease.standard] } };
/** Scale rides the spring; opacity and blur tween so blur never overshoots below zero. */
const iconEnter = { ...motionTokens.spring.snappy, opacity: { duration: motionTokens.duration.fast, ease: [...motionTokens.ease.enter] }, filter: { duration: motionTokens.duration.fast, ease: [...motionTokens.ease.enter] } } as const;

/** Names the icon element, so swapping Copy for Check morphs while a re-render of the same icon stays still. */
function iconKey(node: ReactNode): string {
  if (!isValidElement(node)) return node == null || typeof node === "boolean" ? "" : String(node);
  const type = node.type as string | { displayName?: string; name?: string };
  return typeof type === "string" ? type : type?.displayName ?? type?.name ?? "icon";
}

/** The main action morphs its icon and label in place while its width follows on a spring; the menu half never scales, so the menu opens from a still anchor. */
export function SplitButton({ label, actions, onClick, disabled, icon, variant = "primary" }: SplitButtonProps) {
  const reduced = useReducedMotion() ?? false;
  const { highlight, reset, contentProps } = useMenuHighlight();
  const glyph = iconKey(icon);
  return <DropdownPrimitive.Root onOpenChange={open => { if (open) reset(); }}>
    <div className={[styles.group, variant === "secondary" ? styles.secondary : ""].filter(Boolean).join(" ")}>
      <button className={styles.primary} type="button" onClick={onClick} disabled={disabled}>
        <span className={styles.primarySlot} aria-hidden="true">
          <span className={styles.primaryContent}>
            {icon ? <span className={styles.mainIcon}><AnimatePresence initial={false}><motion.span key={glyph} className={styles.iconPhase} initial={reduced ? fadeIn : iconIn} animate={rest} exit={reduced ? fadeOut : iconOut} transition={reduced ? { duration: motionTokens.duration.instant } : iconEnter}>{icon}</motion.span></AnimatePresence></span> : null}
            <TextMorph>{label}</TextMorph>
          </span>
        </span>
        <span className={styles.srOnly} aria-live="polite">{label}</span>
      </button>
      <DropdownPrimitive.Trigger className={styles.trigger} type="button" aria-label={`${label} more actions`} disabled={disabled}><ChevronDown className={styles.chevron} size={16} strokeWidth={1.75} aria-hidden="true" /></DropdownPrimitive.Trigger>
    </div>
    <DropdownPrimitive.Portal><DropdownPrimitive.Content className={styles.menu} sideOffset={4} align="end" collisionPadding={12} loop {...contentProps}>
      <MenuHighlight state={highlight} className={styles.highlight} />
      {actions.map((action, index) => <DropdownPrimitive.Item key={action.label} className={[styles.item, action.destructive ? styles.destructive : ""].filter(Boolean).join(" ")} data-tone={action.destructive ? "danger" : undefined} style={{ "--i": index } as CSSProperties} disabled={action.disabled} onSelect={action.onSelect}>{action.icon ? <span className={styles.icon} aria-hidden="true">{action.icon}</span> : null}{action.label}</DropdownPrimitive.Item>)}
    </DropdownPrimitive.Content></DropdownPrimitive.Portal>
  </DropdownPrimitive.Root>;
}

export default SplitButton;
