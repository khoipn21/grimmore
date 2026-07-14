const SYNCHRONOUS_LOAD_MEASURE = "grimmore-plugin-synchronous-load";

export function recordSynchronousLoad(startedAt: number): void {
  performance.measure(SYNCHRONOUS_LOAD_MEASURE, {
    start: startedAt,
    end: performance.now(),
  });
}

export { SYNCHRONOUS_LOAD_MEASURE };
