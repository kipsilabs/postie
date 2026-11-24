export interface DurationParts {
	number: number;
	unit: string;
}

const FIRST_UNIT = /^(\d+)([smh])/;

/**
 * Splits a duration such as "5m0s" into the number and unit shown by DurationInput (its first
 * unit). An empty or unparsable value shows `fallback`, which should be the value the backend uses
 * for an empty duration, so the form does not display, and then save, something else.
 */
export function parseDurationInput(value: string | undefined, fallback = "5s"): DurationParts {
	const match = typeof value === "string" ? value.trim().match(FIRST_UNIT) : null;
	if (match) {
		return { number: Number.parseInt(match[1]), unit: match[2] };
	}
	const fb = fallback.match(FIRST_UNIT);
	return fb ? { number: Number.parseInt(fb[1]), unit: fb[2] } : { number: 5, unit: "s" };
}
