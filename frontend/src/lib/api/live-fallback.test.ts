import { describe, expect, test } from "bun:test";
import { startLiveFallback } from "./live-fallback";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("startLiveFallback", () => {
	test("refreshes on the interval while the live channel is down", async () => {
		let calls = 0;
		const stop = startLiveFallback({
			intervalMs: 10,
			isLive: () => false,
			refresh: () => {
				calls++;
			},
		});
		await sleep(65);
		stop();
		expect(calls).toBeGreaterThanOrEqual(3);
	});

	test("does not refresh while the live channel is up", async () => {
		let calls = 0;
		const stop = startLiveFallback({
			intervalMs: 10,
			isLive: () => true,
			refresh: () => {
				calls++;
			},
		});
		await sleep(50);
		stop();
		expect(calls).toBe(0);
	});

	test("refreshes once immediately when the live channel reconnects", async () => {
		let calls = 0;
		let fire: (() => void) | undefined;
		const stop = startLiveFallback({
			intervalMs: 10_000,
			isLive: () => true,
			refresh: () => {
				calls++;
			},
			onReconnect: (cb) => {
				fire = cb;
				return () => {
					fire = undefined;
				};
			},
		});
		fire?.();
		await sleep(5);
		expect(calls).toBe(1);
		stop();
		expect(fire).toBeUndefined();
	});

	test("stop halts further refreshes", async () => {
		let calls = 0;
		const stop = startLiveFallback({
			intervalMs: 10,
			isLive: () => false,
			refresh: () => {
				calls++;
			},
		});
		await sleep(25);
		stop();
		const after = calls;
		await sleep(40);
		expect(calls).toBe(after);
	});
});
