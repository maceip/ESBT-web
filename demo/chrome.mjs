/** Shared Chrome flags for the three harnesses. */
const DEFAULT_CHROME =
  process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "/opt/google/chrome/chrome";

export const CHROME = process.env.CHROME_PATH || DEFAULT_CHROME;

export const CHROME_ARGS = [
  "--no-sandbox",
  "--disable-dev-shm-usage",
  "--disable-gpu",
  "--disable-extensions",
  "--no-first-run",
  "--no-default-browser-check",
];
