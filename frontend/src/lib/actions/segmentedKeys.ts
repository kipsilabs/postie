/**
 * Arrow-key roving for a `role="radiogroup"` of `role="radio"` buttons.
 * Left/Up and Right/Down move focus to the neighbour and select it (click);
 * Home/End jump to the ends. Pair with `tabindex={checked ? 0 : -1}` on items.
 */
export function segmentedKeys(node: HTMLElement) {
	function onKeydown(event: KeyboardEvent) {
		const items = Array.from(node.querySelectorAll<HTMLElement>('[role="radio"]'));
		const current = items.indexOf(document.activeElement as HTMLElement);
		if (current < 0 || items.length === 0) return;

		let next = current;
		switch (event.key) {
			case "ArrowRight":
			case "ArrowDown":
				next = (current + 1) % items.length;
				break;
			case "ArrowLeft":
			case "ArrowUp":
				next = (current - 1 + items.length) % items.length;
				break;
			case "Home":
				next = 0;
				break;
			case "End":
				next = items.length - 1;
				break;
			default:
				return;
		}
		event.preventDefault();
		items[next].focus();
		items[next].click();
	}

	node.addEventListener("keydown", onKeydown);
	return {
		destroy() {
			node.removeEventListener("keydown", onKeydown);
		},
	};
}
