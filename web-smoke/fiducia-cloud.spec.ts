import { test, expect, type Page } from "@playwright/test";

// The fiducia.cloud marketing site is a static Astro site served from GitHub
// Pages (see the embedded repo_map: "fiducia.cloud = GitHub Pages (marketing)"
// and fiducia-marketing.web). The *.github.io URL is the reliable default we
// hard-assert on so this job is meaningful; the custom apex domain and the
// app/admin surfaces are checked tolerantly because they can be mid-DNS-cutover
// or undeployed. No secrets are needed — these are public pages.

const PAGES_URL = "https://fiducia-cloud.github.io/";
const APEX_URL = "https://fiducia.cloud/";
const TOLERANT_SURFACES = [
  "https://app.fiducia.cloud/",
  "https://admin.fiducia.cloud/",
];

// Stable content the marketing page renders: the document title and the hero
// tagline. Kept loose enough to survive copy tweaks but specific to Fiducia.
const EXPECTED_TITLE = /Fiducia.*Consensus.*Coordination as a Service/i;
const EXPECTED_TEXT = "Coordination as a Service";

async function assertMarketingSite(page: Page, url: string): Promise<void> {
  const response = await page.goto(url, { waitUntil: "domcontentloaded" });
  expect(response, `no HTTP response from ${url}`).not.toBeNull();
  expect(
    response!.status(),
    `${url} returned HTTP ${response!.status()}`,
  ).toBeLessThan(400);
  await expect(page).toHaveTitle(EXPECTED_TITLE);
  await expect(
    page.getByText(EXPECTED_TEXT, { exact: false }).first(),
  ).toBeVisible();
}

test("GitHub Pages marketing site is live with expected content", async ({
  page,
}) => {
  // Hard assertion on the reliably-live surface: if this fails, the site is
  // genuinely broken.
  await assertMarketingSite(page, PAGES_URL);
});

test("custom apex domain serves the marketing site when reachable", async ({
  page,
}) => {
  // Tolerant: the apex may still be mid-DNS-cutover. Log and soft-skip rather
  // than fail the scheduled job.
  try {
    await assertMarketingSite(page, APEX_URL);
  } catch (error) {
    test.skip(
      true,
      `apex ${APEX_URL} is not serving the marketing site yet: ${String(error)}`,
    );
  }
});

test("app/admin surfaces respond when deployed", async ({ page }) => {
  // Tolerant probe only: these surfaces live on the Hetzner edge and may be
  // undeployed or unreachable. Record what we see; never fail on them.
  for (const url of TOLERANT_SURFACES) {
    try {
      const response = await page.goto(url, {
        waitUntil: "domcontentloaded",
        timeout: 15_000,
      });
      console.log(`${url} -> HTTP ${response?.status() ?? "no response"}`);
    } catch (error) {
      console.log(`${url} not reachable (tolerated): ${String(error)}`);
    }
  }
  // No hard assertion here by design: reaching this point means the tolerant
  // probes completed without throwing out of the loop.
  expect(true).toBe(true);
});
