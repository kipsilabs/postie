import { Activity, ChartPie, FileText, Settings } from "lucide-svelte";

export interface NavItem {
	id: string;
	href: string;
	/** i18n key for the label / page title */
	labelKey: string;
	icon: typeof Settings;
	/** disabled while configuration is missing or broken */
	requiresConfig: boolean;
}

export const NAV_ITEMS: readonly NavItem[] = [
	{ id: "/", href: "/", labelKey: "common.nav.dashboard", icon: ChartPie, requiresConfig: true },
	{ id: "/settings", href: "/settings", labelKey: "common.nav.settings", icon: Settings, requiresConfig: false },
	{ id: "/metrics", href: "/metrics", labelKey: "common.nav.metrics", icon: Activity, requiresConfig: true },
	{ id: "/logs", href: "/logs", labelKey: "common.nav.logs", icon: FileText, requiresConfig: false },
];
