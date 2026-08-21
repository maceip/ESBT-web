/** Shared Chrome flags for the three harnesses. */
export const CHROME = process.env.CHROME_PATH || "/opt/google/chrome/chrome";

export const CHROME_ARGS = [
  "--no-sandbox",
  "--disable-dev-shm-usage",
  "--disable-gpu",
  "--disable-extensions",
  "--no-first-run",
  "--no-default-browser-check",
];
