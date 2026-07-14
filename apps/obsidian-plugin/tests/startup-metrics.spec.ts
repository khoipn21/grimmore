import { afterEach, describe, expect, it } from "vitest";

import {
  recordSynchronousLoad,
  SYNCHRONOUS_LOAD_MEASURE,
} from "../src/startup-metrics.js";

afterEach(() => {
  performance.clearMeasures(SYNCHRONOUS_LOAD_MEASURE);
});

describe("plugin startup metrics", () => {
  it("records a finite synchronous-load duration", () => {
    recordSynchronousLoad(performance.now());

    const entries = performance.getEntriesByName(SYNCHRONOUS_LOAD_MEASURE);
    const entry = entries.at(-1);
    expect(entry).toBeDefined();
    expect(entry?.duration).toBeGreaterThanOrEqual(0);
    expect(Number.isFinite(entry?.duration)).toBe(true);
  });
});
