import type { AnchorHTMLAttributes, ComponentType, ReactNode } from "react";
import buttonStyles from "../button/button.module.css";
import type { ButtonSize, ButtonVariant } from "../button/button";
import styles from "./button-link.module.css";

export interface ButtonLinkProps extends AnchorHTMLAttributes<HTMLAnchorElement> {
  href: string;
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** The element to render instead of <a>, such as next/link. */
  as?: ComponentType<AnchorHTMLAttributes<HTMLAnchorElement> & { href: string }>;
  children: ReactNode;
}

export function ButtonLink({ href, variant = "primary", size = "md", as: Anchor, className, children, ...props }: ButtonLinkProps) {
  const classes = [buttonStyles.button, buttonStyles[variant], buttonStyles[size], styles.link, className].filter(Boolean).join(" ");
  const content = <span className={styles.content}>{children}</span>;
  if (Anchor) return <Anchor {...props} href={href} data-variant={variant} className={classes}>{content}</Anchor>;
  return <a {...props} href={href} data-variant={variant} className={classes}>{content}</a>;
}

export default ButtonLink;
