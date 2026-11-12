<script lang="ts">
import LanguageSwitcher from "$lib/components/LanguageSwitcher.svelte";
import ThemeSwitcher from "$lib/components/ThemeSwitcher.svelte";
import { t } from "$lib/i18n";
</script>

<div class="mx-auto flex w-full max-w-xl flex-col gap-6">
	<div class="space-y-2 text-center">
		<h2 class="text-2xl font-semibold text-base-content">
			{$t("setup.welcome.title")}
		</h2>
		<p class="text-base text-base-content/65">
			{$t("setup.welcome.subtitle")}
		</p>
	</div>

	<div>
		<div class="section-label">{$t("setup.welcome.preferences_title")}</div>
		<div class="settings-group">
			<div class="settings-row flex-wrap">
				<div class="min-w-0">
					<div class="settings-row-title">{$t("setup.welcome.language_title")}</div>
					<div class="settings-row-desc">{$t("setup.welcome.language_description")}</div>
				</div>
				<div class="w-full shrink-0 sm:w-48">
					<LanguageSwitcher />
				</div>
			</div>
			<div class="settings-row flex-wrap">
				<div class="min-w-0">
					<div class="settings-row-title">{$t("setup.welcome.theme_title")}</div>
					<div class="settings-row-desc">{$t("setup.welcome.theme_description")}</div>
				</div>
				<ThemeSwitcher />
			</div>
		</div>
	</div>
</div>
