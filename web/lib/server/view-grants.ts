/** Opaque email receipts convey public viewing only, never account identity. */
export const viewGrantCookie = (id: string) => `calc-view-${id}`;
export function viewGrantHeaders(cookies: { getAll(): { name: string; value: string }[] }): Record<string, string> {
  const grants = cookies.getAll().filter(cookie => /^calc-view-[a-z0-9]+$/.test(cookie.name) && /^[a-f0-9]{64}$/.test(cookie.value)).slice(-20).map(cookie => cookie.value);
  return grants.length ? { "x-calc-view-grants": grants.join(",") } : {};
}
