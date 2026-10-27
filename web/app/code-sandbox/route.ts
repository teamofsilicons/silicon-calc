import { CODE_SANDBOX_CSP, CODE_SANDBOX_HTML } from "@/lib/sheet/code-sandbox";

export function GET() {
  return new Response(CODE_SANDBOX_HTML, { headers: {
    "Content-Type": "text/html; charset=utf-8",
    "Content-Security-Policy": CODE_SANDBOX_CSP,
    "Cache-Control": "no-store",
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Permissions-Policy": "camera=(), microphone=(), geolocation=(), payment=()",
  } });
}
