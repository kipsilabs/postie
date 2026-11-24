import { describe, expect, test } from "bun:test";
import { parseDurationInput } from "./duration-input";

describe("parseDurationInput", () => {
	test("reads the first unit of a Go-style duration", () => {
		expect(parseDurationInput("5m0s")).toEqual({ number: 5, unit: "m" });
		expect(parseDurationInput("30s")).toEqual({ number: 30, unit: "s" });
		expect(parseDurationInput("0s")).toEqual({ number: 0, unit: "s" });
	});

	test("an empty value shows the given fallback, not a hard-coded 5 seconds", () => {
		// an empty watcher check interval means 60 s to the backend
		expect(parseDurationInput("", "1m")).toEqual({ number: 1, unit: "m" });
		expect(parseDurationInput(undefined, "0s")).toEqual({ number: 0, unit: "s" });
	});

	test("without a fallback an empty value keeps the old 5 second default", () => {
		expect(parseDurationInput("")).toEqual({ number: 5, unit: "s" });
	});

	test("an unparsable value also uses the fallback", () => {
		expect(parseDurationInput("soon", "1m")).toEqual({ number: 1, unit: "m" });
	});
});
