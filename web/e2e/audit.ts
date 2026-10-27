import { join } from "node:path";
import { expect, type Page } from "@playwright/test";

const AXE = join(process.cwd(), "node_modules", "axe-core", "axe.min.js");
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

export async function audit(page: Page, label: string) {
  await page.waitForLoadState("domcontentloaded");
  // Measure the page at rest: a panel fading in has its text at partial opacity, which is not what anyone reads.
  await page.waitForFunction(() => document.getAnimations().every(animation => animation.playState !== "running"), undefined, { timeout: 5_000 }).catch(() => undefined);
  await page.waitForTimeout(500);
  await page.addScriptTag({ path: AXE });
  const result = await page.evaluate(async tags => {
    const axe = (window as unknown as { axe: { run: (context: Document, options: object) => Promise<{ violations: Array<{ id: string; impact: string; help: string; nodes: Array<{ target: string[]; failureSummary: string }> }> }> } }).axe;
    return axe.run(document, { runOnly: { type: "tag", values: tags }, resultTypes: ["violations"] });
  }, TAGS);
  const problems = result.violations.map(violation => `${violation.id} (${violation.impact}): ${violation.help}\n${violation.nodes.slice(0, 4).map(node => `    ${node.target.join(" ")}: ${node.failureSummary.replace(/\s+/g, " ")}`).join("\n")}`);
  expect(problems, `${label}\n${problems.join("\n")}`).toEqual([]);
}

