import { describe, expect, test } from "bun:test";
import { classifyProvider, type ProviderSnapshot } from "./provider-status";

const snap = (over: Partial<ProviderSnapshot>): ProviderSnapshot => ({
	activeConnections: 0,
	totalErrors: 0,
	...over,
});

describe("classifyProvider", () => {
	test("connected while it holds active connections", () => {
		expect(classifyProvider(snap({ activeConnections: 3, totalErrors: 500 }))).toBe("connected");
	});

	test("idle when nothing is in flight, even with historical errors", () => {
		const previous = snap({ totalErrors: 109 });
		expect(classifyProvider(snap({ totalErrors: 109 }), previous)).toBe("idle");
	});

	test("idle with no history to compare against", () => {
		expect(classifyProvider(snap({ totalErrors: 109 }))).toBe("idle");
	});

	test("failed when errors keep growing without any connection", () => {
		const previous = snap({ totalErrors: 100 });
		expect(classifyProvider(snap({ totalErrors: 104 }), previous)).toBe("failed");
	});
});
