/**
 * GET /og.png: the app's Open Graph image (1200 by 630), in Calc's look: Paper Mono on a white page,
 * the mark and name, the landing page's headline, and
 * the command that installs it. Everything comes from lib/app.config.ts, so it is drawn once at build time.
 * Paper Mono is read from assets/og/paper-mono (TrueType: next/og cannot read the site's WOFF2 file; SIL OFL 1.1).
 */
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { ImageResponse } from "next/og";
import { appConfig } from "@/lib/app.config";
import { markSvg, svgDataUri } from "@/lib/brand-svg";

export const dynamic = "force-static";

const PAGE = "#FFFFFF";
const INK = "#353535";
const SECONDARY = "#707070";
const BORDER = "#E5E5E5";

const fonts = (async () => {
  const dir = join(process.cwd(), "assets", "og", "paper-mono");
  const [regular, medium, semibold] = await Promise.all(["PaperMono-Regular.ttf", "PaperMono-Medium.ttf", "PaperMono-SemiBold.ttf"].map(name => readFile(join(dir, name))));
  return [
    { name: "Paper Mono", data: regular, weight: 400 as const, style: "normal" as const },
    { name: "Paper Mono", data: medium, weight: 500 as const, style: "normal" as const },
    { name: "Paper Mono", data: semibold, weight: 600 as const, style: "normal" as const },
  ];
})();

const clip = (text: string, max: number) => (text.length <= max ? text : `${text.slice(0, max - 1).trimEnd()}…`);

export async function GET() {
  return new ImageResponse(
    (
      <div style={{ width: "100%", height: "100%", display: "flex", flexDirection: "column", padding: "64px 76px", backgroundColor: PAGE, color: INK, fontFamily: "Paper Mono" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 18, fontSize: 34, fontWeight: 600, letterSpacing: -0.7 }}>
          {/* eslint-disable-next-line @next/next/no-img-element -- next/og draws plain elements */}
          <img src={svgDataUri(markSvg())} width={60} height={60} alt="" />
          {appConfig.brandPrefix ? <span>{appConfig.brandPrefix}</span> : null}
          <span style={appConfig.brandPrefix ? { color: SECONDARY, fontWeight: 500, marginLeft: -6 } : undefined}>{appConfig.name}</span>
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 22, marginTop: 70, maxWidth: 940 }}>
          <div style={{ fontSize: 72, fontWeight: 600, letterSpacing: -2.6, lineHeight: 1.02 }}>{clip(appConfig.landing.headline, 70)}</div>
          <div style={{ fontSize: 30, color: SECONDARY, lineHeight: 1.35 }}>{clip(appConfig.tagline, 110)}</div>
        </div>
        <div style={{ display: "flex", marginTop: "auto", alignItems: "center", justifyContent: "space-between" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 14, padding: "14px 22px", border: `1.5px solid ${BORDER}`, borderRadius: 20, backgroundColor: "#FFFFFF", fontSize: 26, fontWeight: 500 }}>
            <span style={{ color: INK, fontWeight: 600 }}>$</span>
            <span>{`silicon-apps install ${appConfig.appId}`}</span>
          </div>
          <div style={{ display: "flex", fontSize: 24, color: SECONDARY }}>For Carbons and Silicons</div>
        </div>
      </div>
    ),
    { width: 1200, height: 630, fonts: await fonts, headers: { "Cache-Control": "public, max-age=3600, stale-while-revalidate=86400" } },
  );
}
