import { defineConfig, devices } from "@playwright/test";

// Minimal Playwright config for the fiducia.cloud browser-smoke. The only
// external dependency is the target site itself; there is no local web server
// to start. Retries absorb transient network blips against the live surface.
export default defineConfig({
  testDir: ".",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 2,
  workers: 1,
  reporter: [["list"]],
  timeout: 45_000,
  expect: { timeout: 15_000 },
  use: {
    ...devices["Desktop Chrome"],
    headless: true,
    navigationTimeout: 30_000,
    actionTimeout: 15_000,
  },
});
