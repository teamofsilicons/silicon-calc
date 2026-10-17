"use client";

/**
 * Dropdown menu: a list of actions behind a trigger.
 *
 * Two ways to use it:
 *
 *   // Shorthand: a labelled trigger and a flat list.
 *   <DropdownMenu label="Manage silicon" items={[{ label: "Rename", onSelect: rename }, …]} />
 *
 *   // Composable (experimental): any trigger, labels, checked and radio items, shortcuts and submenus.
 *   <DropdownMenuRoot>
 *     <DropdownMenuTrigger asChild><Button variant="ghost" aria-label="Page options"><Ellipsis /></Button></DropdownMenuTrigger>
 *     <DropdownMenuContent align="end">
 *       <DropdownMenuLabel>Page</DropdownMenuLabel>
 *       <DropdownMenuItem icon={<Copy />} shortcut="⌘D" onSelect={duplicate}>Duplicate</DropdownMenuItem>
 *       <DropdownMenuCheckboxItem checked={wide} onCheckedChange={setWide}>Full width</DropdownMenuCheckboxItem>
 *       <DropdownMenuSub>
 *         <DropdownMenuSubTrigger icon={<FolderInput />}>Move to</DropdownMenuSubTrigger>
 *         <DropdownMenuSubContent>…</DropdownMenuSubContent>
 *       </DropdownMenuSub>
 *       <DropdownMenuSeparator />
 *       <DropdownMenuItem destructive icon={<Trash2 />} onSelect={remove}>Delete</DropdownMenuItem>
 *     </DropdownMenuContent>
 *   </DropdownMenuRoot>
 *
 * Every part shares one look: the same surface, the gliding highlight, the entrance that grows from the trigger.
 */
import { Fragment, forwardRef, useImperativeHandle, useLayoutEffect, useRef, useState } from "react";
import type { ComponentPropsWithoutRef, ReactNode, Ref } from "react";
import * as DropdownPrimitive from "@radix-ui/react-dropdown-menu";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { Check, ChevronDown, ChevronRight } from "lucide-react";
import { MenuHighlight, useMenuHighlight } from "../lib/menu-highlight";
import { motionTokens } from "../lib/motion-tokens";
import styles from "./dropdown-menu.module.css";

const ITEMS = '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]';
const cx = (...names: Array<string | false | undefined>) => names.filter(Boolean).join(" ");

/** A menu surface's own ref, also handed to the caller's ref. It numbers the items in order, so their entrance
 *  staggers whatever groups and parts they sit in. */
function useSurfaceRef(forwarded: Ref<HTMLDivElement>) {
  const own = useRef<HTMLDivElement>(null);
  useImperativeHandle(forwarded, () => own.current as HTMLDivElement, []);
  useLayoutEffect(() => {
    own.current?.querySelectorAll<HTMLElement>(ITEMS).forEach((item, index) => item.style.setProperty("--i", String(index)));
  });
  return own;
}

/* ------------------------------------------------------------------------------------------------------------------ */
/* Composable parts                                                                                                    */
/* ------------------------------------------------------------------------------------------------------------------ */

/** The menu's state. Uncontrolled by default; pass `open` and `onOpenChange` to control it. */
export function DropdownMenuRoot(props: ComponentPropsWithoutRef<typeof DropdownPrimitive.Root>) {
  return <DropdownPrimitive.Root {...props} />;
}

export interface DropdownMenuTriggerProps extends ComponentPropsWithoutRef<typeof DropdownPrimitive.Trigger> {
  /** Render your own trigger (an icon-only Button with an aria-label, an avatar and name) instead of the default one. */
  asChild?: boolean;
}

/** What opens the menu. Without `asChild` it is the default outlined trigger with a chevron. */
export const DropdownMenuTrigger = forwardRef<HTMLButtonElement, DropdownMenuTriggerProps>(function DropdownMenuTrigger({ asChild, className, children, ...props }, ref) {
  if (asChild) return <DropdownPrimitive.Trigger {...props} ref={ref} asChild>{children}</DropdownPrimitive.Trigger>;
  return (
    <DropdownPrimitive.Trigger {...props} ref={ref} type="button" className={cx(styles.trigger, className)}>
      {children}
      <ChevronDown className={styles.chevron} size={15} strokeWidth={1.8} aria-hidden="true" />
    </DropdownPrimitive.Trigger>
  );
});

export interface DropdownMenuContentProps extends ComponentPropsWithoutRef<typeof DropdownPrimitive.Content> {
  /** Which side of the trigger it opens on (default bottom). */
  side?: "top" | "right" | "bottom" | "left";
  /** How it lines up with the trigger (default end). */
  align?: "start" | "center" | "end";
}

/** The menu itself. One highlight glides between items for the pointer and jumps instantly for the keyboard. */
export const DropdownMenuContent = forwardRef<HTMLDivElement, DropdownMenuContentProps>(function DropdownMenuContent({ className, children, side = "bottom", align = "end", sideOffset = 6, collisionPadding = 12, ...props }, ref) {
  const { highlight, contentProps } = useMenuHighlight(ITEMS);
  const surface = useSurfaceRef(ref);
  return (
    <DropdownPrimitive.Portal>
      <DropdownPrimitive.Content {...props} {...contentProps} ref={surface} side={side} align={align} sideOffset={sideOffset} collisionPadding={collisionPadding} loop className={cx(styles.menu, className)}>
        <MenuHighlight state={highlight} className={styles.highlight} />
        {children}
      </DropdownPrimitive.Content>
    </DropdownPrimitive.Portal>
  );
});

export interface DropdownMenuItemProps extends ComponentPropsWithoutRef<typeof DropdownPrimitive.Item> {
  /** A 16px icon before the label. */
  icon?: ReactNode;
  /** A keyboard shortcut hint after the label, such as "⌘D". Shown only; bind the keys yourself. */
  shortcut?: string;
  /** A destructive action (delete, remove): shown in the danger colour. */
  destructive?: boolean;
}

/** One action. */
export const DropdownMenuItem = forwardRef<HTMLDivElement, DropdownMenuItemProps>(function DropdownMenuItem({ icon, shortcut, destructive, className, children, ...props }, ref) {
  return (
    <DropdownPrimitive.Item {...props} ref={ref} className={cx(styles.item, destructive && styles.destructive, className)} data-tone={destructive ? "danger" : undefined}>
      {icon ? <span className={styles.icon} aria-hidden="true">{icon}</span> : null}
      <span className={styles.itemLabel}>{children}</span>
      {shortcut ? <kbd className={styles.shortcut}>{shortcut}</kbd> : null}
    </DropdownPrimitive.Item>
  );
});

export interface DropdownMenuCheckboxItemProps extends ComponentPropsWithoutRef<typeof DropdownPrimitive.CheckboxItem> {
  /** A keyboard shortcut hint after the label. */
  shortcut?: string;
}

/** An on/off setting, such as "Full width". A check shows in the icon column while it is on. */
export const DropdownMenuCheckboxItem = forwardRef<HTMLDivElement, DropdownMenuCheckboxItemProps>(function DropdownMenuCheckboxItem({ shortcut, className, children, ...props }, ref) {
  return (
    <DropdownPrimitive.CheckboxItem {...props} ref={ref} className={cx(styles.item, className)}>
      <span className={styles.icon} aria-hidden="true"><DropdownPrimitive.ItemIndicator><Check size={16} strokeWidth={2} /></DropdownPrimitive.ItemIndicator></span>
      <span className={styles.itemLabel}>{children}</span>
      {shortcut ? <kbd className={styles.shortcut}>{shortcut}</kbd> : null}
    </DropdownPrimitive.CheckboxItem>
  );
});

/** One choice out of several (the current space, a sort order). Wrap DropdownMenuRadioItems in it. */
export const DropdownMenuRadioGroup = DropdownPrimitive.RadioGroup;

export interface DropdownMenuRadioItemProps extends ComponentPropsWithoutRef<typeof DropdownPrimitive.RadioItem> {
  /** A 16px icon or avatar before the label; the check shows at the end while this is the current choice. */
  icon?: ReactNode;
}

/** One of the choices in a DropdownMenuRadioGroup. The current choice shows a check at the end. */
export const DropdownMenuRadioItem = forwardRef<HTMLDivElement, DropdownMenuRadioItemProps>(function DropdownMenuRadioItem({ icon, className, children, ...props }, ref) {
  return (
    <DropdownPrimitive.RadioItem {...props} ref={ref} className={cx(styles.item, className)}>
      {icon ? <span className={styles.icon} aria-hidden="true">{icon}</span> : null}
      <span className={styles.itemLabel}>{children}</span>
      <span className={styles.indicator} aria-hidden="true"><DropdownPrimitive.ItemIndicator><Check size={16} strokeWidth={2} /></DropdownPrimitive.ItemIndicator></span>
    </DropdownPrimitive.RadioItem>
  );
});

/** A section heading inside the menu, such as "Spaces". Not focusable. */
export const DropdownMenuLabel = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<typeof DropdownPrimitive.Label>>(function DropdownMenuLabel({ className, ...props }, ref) {
  return <DropdownPrimitive.Label {...props} ref={ref} className={cx(styles.sectionLabel, className)} />;
});

/** A hairline between groups of items. */
export const DropdownMenuSeparator = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<typeof DropdownPrimitive.Separator>>(function DropdownMenuSeparator({ className, ...props }, ref) {
  return <DropdownPrimitive.Separator {...props} ref={ref} className={cx(styles.separator, className)} />;
});

/** Groups items for assistive technology, without visual change. */
export const DropdownMenuGroup = DropdownPrimitive.Group;

/** A nested menu: a DropdownMenuSubTrigger and its DropdownMenuSubContent. */
export const DropdownMenuSub = DropdownPrimitive.Sub;

export interface DropdownMenuSubTriggerProps extends ComponentPropsWithoutRef<typeof DropdownPrimitive.SubTrigger> {
  /** A 16px icon before the label. */
  icon?: ReactNode;
}

/** The item that opens a submenu (on hover, click, or the right arrow key). */
export const DropdownMenuSubTrigger = forwardRef<HTMLDivElement, DropdownMenuSubTriggerProps>(function DropdownMenuSubTrigger({ icon, className, children, ...props }, ref) {
  return (
    <DropdownPrimitive.SubTrigger {...props} ref={ref} className={cx(styles.item, styles.subTrigger, className)}>
      {icon ? <span className={styles.icon} aria-hidden="true">{icon}</span> : null}
      <span className={styles.itemLabel}>{children}</span>
      <ChevronRight className={styles.subChevron} size={15} strokeWidth={1.8} aria-hidden="true" />
    </DropdownPrimitive.SubTrigger>
  );
});

/** The submenu's own menu, opening beside its trigger. */
export const DropdownMenuSubContent = forwardRef<HTMLDivElement, ComponentPropsWithoutRef<typeof DropdownPrimitive.SubContent>>(function DropdownMenuSubContent({ className, children, sideOffset = 6, collisionPadding = 12, ...props }, ref) {
  const { highlight, contentProps } = useMenuHighlight(ITEMS);
  const surface = useSurfaceRef(ref);
  return (
    <DropdownPrimitive.Portal>
      <DropdownPrimitive.SubContent {...props} {...contentProps} ref={surface} sideOffset={sideOffset} collisionPadding={collisionPadding} loop className={cx(styles.menu, className)}>
        <MenuHighlight state={highlight} className={styles.highlight} />
        {children}
      </DropdownPrimitive.SubContent>
    </DropdownPrimitive.Portal>
  );
});

/* ------------------------------------------------------------------------------------------------------------------ */
/* Shorthand                                                                                                           */
/* ------------------------------------------------------------------------------------------------------------------ */

export interface DropdownItem {
  /** The item's text. */
  label: string;
  /** Runs when the item is chosen. */
  onSelect?: () => void;
  /** Shown but not choosable. */
  disabled?: boolean;
  /** A 16px icon before the label. */
  icon?: ReactNode;
  /** A destructive action, shown in the danger colour. */
  destructive?: boolean;
  /** Draws a separator above this item. */
  separatorBefore?: boolean;
}

export interface DropdownMenuProps {
  /** The trigger's text; when it names the current choice, a new label rises in and the trigger springs to its width. */
  label: string;
  /** The actions, in order. */
  items: DropdownItem[];
  /** An icon before the trigger's text. */
  icon?: ReactNode;
}

/** A new trigger label rises in while the old one leaves, and the trigger width springs to the measured text instead of snapping. */
function TriggerLabel({ text }: { text: string }) {
  const reduced = useReducedMotion();
  const measure = useRef<HTMLSpanElement>(null);
  const measured = useRef<string | null>(null);
  const [size, setSize] = useState<{ width: number | "auto"; animate: boolean }>({ width: "auto", animate: false });
  useLayoutEffect(() => {
    const node = measure.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const current = node.textContent;
      // Only a text change morphs; the first measure and font swaps settle instantly.
      const animate = measured.current !== null && measured.current !== current;
      measured.current = current;
      setSize({ width: Math.ceil(entry.borderBoxSize?.[0]?.inlineSize ?? node.offsetWidth), animate });
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  return <motion.span className={styles.label} initial={false} animate={{ width: size.width }} transition={size.animate && !reduced ? motionTokens.spring.morph : { duration: 0 }}>
    <span ref={measure} className={styles.labelMeasure} aria-hidden="true">{text}</span>
    <AnimatePresence mode="popLayout" initial={false}>
      <motion.span key={text} className={styles.labelText} initial={reduced ? false : { opacity: 0, y: "0.3em", filter: `blur(${motionTokens.blur.soft}px)` }} animate={{ opacity: 1, y: 0, filter: "blur(0px)" }} exit={reduced ? { opacity: 0, transition: { duration: 0 } } : { opacity: 0, y: "-0.3em", filter: `blur(${motionTokens.blur.subtle}px)`, transition: { duration: motionTokens.duration.fast, ease: [...motionTokens.ease.standard] } }} transition={{ duration: motionTokens.duration.standard, ease: [...motionTokens.ease.enter] }}>{text}</motion.span>
    </AnimatePresence>
  </motion.span>;
}

/** The shorthand: a labelled trigger and a flat list of actions, built from the parts above. */
export function DropdownMenu({ label, items, icon }: DropdownMenuProps) {
  return (
    <DropdownMenuRoot>
      <DropdownMenuTrigger>
        {icon ? <span className={styles.triggerIcon} aria-hidden="true">{icon}</span> : null}
        <TriggerLabel text={label} />
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        {items.map(item => (
          <Fragment key={item.label}>
            {item.separatorBefore ? <DropdownMenuSeparator /> : null}
            <DropdownMenuItem icon={item.icon} destructive={item.destructive} disabled={item.disabled} onSelect={item.onSelect}>{item.label}</DropdownMenuItem>
          </Fragment>
        ))}
      </DropdownMenuContent>
    </DropdownMenuRoot>
  );
}

export default DropdownMenu;
