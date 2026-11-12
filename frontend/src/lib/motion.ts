/**
 * Shared motion helpers. They read the same CSS custom properties
 * (--motion-fast/base/slow, --ease-standard/emphasized/in) that drive the CSS
 * animations, so Svelte transitions match the platform's timing, and they
 * collapse to zero duration when the user prefers reduced motion.
 *
 *   <div transition:fade={motion.fade()} />   // see exports below
 *   <li animate:listFlip>
 */
import { browser } from "$app/environment";
import type { TransitionConfig } from "svelte/transition";
import { fade as svelteFade, fly as svelteFly, scale as svelteScale, slide as svelteSlide } from "svelte/transition";

export type MotionSpeed = "fast" | "base" | "slow";
export type EasingName = "standard" | "emphasized" | "in";
type Easing = (t: number) => number;

const FALLBACK_MS: Record<MotionSpeed, number> = { fast: 120, base: 200, slow: 320 };
const FALLBACK_EASE: Record<EasingName, string> = {
	standard: "cubic-bezier(0.2, 0.8, 0.2, 1)",
	emphasized: "cubic-bezier(0.2, 0.8, 0.2, 1)",
	in: "cubic-bezier(0.4, 0, 1, 1)",
};

export function prefersReducedMotion(): boolean {
	return browser && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function cssVar(name: string): string {
	if (!browser) return "";
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Duration in ms for a speed token (0 under reduced motion). */
export function duration(speed: MotionSpeed = "base"): number {
	if (prefersReducedMotion()) return 0;
	const parsed = Number.parseFloat(cssVar(`--motion-${speed}`));
	return Number.isFinite(parsed) ? parsed : FALLBACK_MS[speed];
}

/** Cubic-bezier easing function (x1,y1,x2,y2) as used by CSS. */
export function cubicBezier(x1: number, y1: number, x2: number, y2: number): Easing {
	const cx = 3 * x1;
	const bx = 3 * (x2 - x1) - cx;
	const ax = 1 - cx - bx;
	const cy = 3 * y1;
	const by = 3 * (y2 - y1) - cy;
	const ay = 1 - cy - by;
	const sampleX = (t: number) => ((ax * t + bx) * t + cx) * t;
	const sampleY = (t: number) => ((ay * t + by) * t + cy) * t;
	const slopeX = (t: number) => (3 * ax * t + 2 * bx) * t + cx;
	return (x: number) => {
		if (x <= 0) return 0;
		if (x >= 1) return 1;
		let t = x;
		for (let i = 0; i < 8; i++) {
			const err = sampleX(t) - x;
			if (Math.abs(err) < 1e-5) return sampleY(t);
			const slope = slopeX(t);
			if (Math.abs(slope) < 1e-6) break;
			t -= err / slope;
		}
		let lo = 0;
		let hi = 1;
		t = x;
		while (lo < hi) {
			const err = sampleX(t);
			if (Math.abs(err - x) < 1e-5) break;
			if (x > err) lo = t;
			else hi = t;
			t = (hi - lo) / 2 + lo;
			if (hi - lo < 1e-6) break;
		}
		return sampleY(t);
	};
}

/** Easing function for an easing token, parsed from its CSS custom property. */
export function easing(name: EasingName = "standard"): Easing {
	const raw = cssVar(`--ease-${name}`) || FALLBACK_EASE[name];
	const match = /cubic-bezier\(([^)]+)\)/.exec(raw);
	const nums = match?.[1].split(",").map((n) => Number.parseFloat(n)) ?? [];
	if (nums.length !== 4 || nums.some((n) => Number.isNaN(n))) {
		return cubicBezier(0.2, 0.8, 0.2, 1);
	}
	return cubicBezier(nums[0], nums[1], nums[2], nums[3]);
}

interface BaseParams {
	speed?: MotionSpeed;
	delay?: number;
	ease?: EasingName;
}

function base(p: BaseParams, defaultEase: EasingName = "standard") {
	return {
		duration: duration(p.speed ?? "base"),
		delay: prefersReducedMotion() ? 0 : (p.delay ?? 0),
		easing: easing(p.ease ?? defaultEase),
	};
}

/** Opacity fade. Usage: `in:fade={motion.fade()}` style via the wrappers below. */
export function fade(node: Element, params: BaseParams = {}): TransitionConfig {
	return svelteFade(node, base(params));
}

/** Fade + small rise/slide. Defaults to an 8px rise. */
export function fly(node: Element, params: BaseParams & { x?: number; y?: number; opacity?: number } = {}): TransitionConfig {
	const { x = 0, y = 8, opacity = 0 } = params;
	return svelteFly(node, { ...base(params), x, y, opacity });
}

/** Fade + scale from `start` (default 0.96). Good for popovers and dialogs. */
export function scale(node: Element, params: BaseParams & { start?: number } = {}): TransitionConfig {
	return svelteScale(node, { ...base(params, "emphasized"), start: params.start ?? 0.96, opacity: 0 });
}

/** Height slide for conditionally shown rows and sections. */
export function collapse(node: Element, params: BaseParams = {}): TransitionConfig {
	const { speed = "base", ease = "standard", delay = 0 } = params;
	return svelteSlide(node, { delay, duration: duration(speed), easing: easing(ease) });
}

/** Sheet / drawer slide from an edge (translates by 100% of the element). */
export function sheet(
	node: Element,
	params: BaseParams & { side?: "left" | "right" | "bottom" | "top" } = {},
): TransitionConfig {
	const { side = "left" } = params;
	const rect = node.getBoundingClientRect();
	const x = side === "left" ? -rect.width : side === "right" ? rect.width : 0;
	const y = side === "top" ? -rect.height : side === "bottom" ? rect.height : 0;
	return svelteFly(node, { ...base({ speed: "slow", ...params }, "emphasized"), x, y, opacity: 1 });
}

/** `animate:listFlip` for keyed `{#each}` lists. */
export function listFlip(
	node: Element,
	{ from, to }: { from: DOMRect; to: DOMRect },
	params: { speed?: MotionSpeed } = {},
): { delay: number; duration: number; easing: Easing; css: (t: number, u: number) => string } {
	const dx = from.left - to.left;
	const dy = from.top - to.top;
	const ease = easing("standard");
	return {
		delay: 0,
		duration: duration(params.speed ?? "base"),
		easing: ease,
		css: (_t, u) => `transform: translate(${dx * u}px, ${dy * u}px)`,
	};
}

/** Plain duration (ms) for `animate:flip={{ duration: flipDuration() }}` style use. */
export function flipDuration(): number {
	return duration("base");
}
