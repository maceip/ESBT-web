import { chromium } from "playwright";
import { CHROME, CHROME_ARGS } from "./chrome.mjs";

export async function launchPlaywright(url) {
  const browser = await chromium.launch({
    executablePath: CHROME,
    headless: true,
    args: CHROME_ARGS,
  });
  const page = await browser.newPage();
  await page.goto(url, { waitUntil: "networkidle" });
  await page.waitForFunction(() => window.__esbtDemo);
  return {
    name: "playwright",
    page,
    async type(text) {
      await page.click("#doc");
      await page.keyboard.type(text, { delay: 15 });
    },
    async backspace(n) {
      await page.click("#doc");
      for (let i = 0; i < n; i++) await page.keyboard.press("Backspace");
    },
    async state() {
      return page.evaluate(() => ({
        text: window.__esbtDemo.text(),
        hash: window.__esbtDemo.hash(),
        len: window.__esbtDemo.len(),
        pending: window.__esbtDemo.pending(),
        site: window.__esbtDemo.site,
      }));
    },
    async verify() {
      return page.evaluate(() => window.__esbtDemo.verify());
    },
    async close() {
      await browser.close();
    },
  };
}
