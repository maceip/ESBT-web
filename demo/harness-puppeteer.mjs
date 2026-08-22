import puppeteer from "puppeteer-core";
import { CHROME, CHROME_ARGS } from "./chrome.mjs";

export async function launchPuppeteer(url) {
  const browser = await puppeteer.launch({
    executablePath: CHROME,
    headless: true,
    args: CHROME_ARGS,
  });
  const page = await browser.newPage();
  await page.goto(url, { waitUntil: "networkidle0" });
  await page.waitForFunction(() => window.__esbtDemo);
  return {
    name: "puppeteer",
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
        diagnostics: window.__esbtDemo.diagnostics(),
      }));
    },
    async close() {
      await browser.close();
    },
  };
}
