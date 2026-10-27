/**
 * The root layout: <html> and <body>, the Silicon look (Silicon UI's foundation, Paper Mono, Calc's light tokens,
 * squircles, the base), the light-only boot script (inline, with the request's CSP nonce from proxy.ts) and the
 * metadata every page shares. It ships no client code of its own: the public pages stay server-rendered HTML with a few
 * small islands, the signed-in pages mount their providers in app/(workspace)/layout.tsx and the sheet editor in
 * app/s/. Every page renders per request (the nonce changes each time).
 */
import "@/components/silicon-ui/foundation.css";
import "@/styles/fonts.css";
import "@/styles/tokens.css";
import "@/styles/squircle.css";
import "@/styles/base.css";
import type { Metadata, Viewport } from "next";
import { headers } from "next/headers";
import type { ReactNode } from "react";
import { appConfig } from "@/lib/app.config";
import { publicOrigin } from "@/lib/server/site";
import { ImportProgress } from "@/components/import/import-progress";
import { THEME_BOOT_SCRIPT } from "@/lib/theme";

export async function generateMetadata(): Promise<Metadata> {
  const origin = publicOrigin();
  return {
    metadataBase: new URL(origin),
    title: { default: appConfig.name, template: `%s · ${appConfig.name}` },
    description: appConfig.description,
    applicationName: appConfig.name,
    referrer: "strict-origin-when-cross-origin",
    // The signed-in pages are private; the landing page opens itself to search engines (app/page.tsx).
    robots: { index: false, follow: false },
    manifest: "/manifest.webmanifest",
    openGraph: { type: "website", siteName: appConfig.name, locale: "en_US", images: [{ url: `${origin}/og.png`, width: 1200, height: 630, alt: `${appConfig.name}: ${appConfig.tagline}` }] },
    twitter: { card: "summary_large_image" },
  };
}

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover",
  themeColor: "#FFFFFF",
  colorScheme: "light",
};

export default async function RootLayout({ children }: { children: ReactNode }) {
  const nonce = (await headers()).get("x-nonce") ?? undefined;
  return (
    <html lang="en" data-theme="light" suppressHydrationWarning>
      <head>
        <script nonce={nonce} dangerouslySetInnerHTML={{ __html: THEME_BOOT_SCRIPT }} />
        {/* The same variable face serves headings, controls, code and the spreadsheet canvas. */}
        <link rel="preload" href="/fonts/paper-mono/v1.000/PaperMono-Variable.woff2" as="font" type="font/woff2" crossOrigin="anonymous" />
      </head>
      <body>{children}<ImportProgress /></body>
    </html>
  );
}
