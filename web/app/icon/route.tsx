/* ImageResponse renders SVG directly; next/image is not available in this image renderer. */
/* eslint-disable @next/next/no-img-element */
/** The tiled install icon. Browser tabs use the transparent, theme-aware app/icon.svg. */
import { ImageResponse } from "next/og";
import { markSvg, svgDataUri } from "@/lib/brand-svg";

const size = { width: 512, height: 512 };
export const dynamic = "force-static";

export function GET() {
  return new ImageResponse(
    <img src={svgDataUri(markSvg())} width={512} height={512} alt="" />,
    size,
  );
}
