<script lang="ts">
import { onMount } from "svelte";
import { t } from "$lib/i18n";
import logo from "$lib/assets/images/logo.png";
import { fade } from "$lib/motion";

let registration = $state(false);
let ready = $state(false);
let busy = $state(false);
let username = $state("");
let email = $state("");
let password = $state("");
let error = $state("");

onMount(async () => {
	try {
		const response = await fetch("/api/auth/registration-status", { signal: AbortSignal.timeout(15000) });
		if (!response.ok) throw new Error();
		const status = await response.json();
		registration = status.registration_enabled;
		ready = true;
	} catch { error = $t("common.auth.connectionError"); }
});

async function submit(event: SubmitEvent) {
	event.preventDefault();
	busy = true;
	error = "";
	try {
		const response = await fetch(`/api/auth/${registration ? "register" : "login"}`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ username, email, password }),
		});
		password = "";
		if (!response.ok) {
			error = $t(response.status === 429 ? "common.auth.tooManyAttempts" : registration ? "common.auth.registrationError" : "common.auth.invalidCredentials");
			return;
		}
		window.location.replace("/");
	} catch { error = $t("common.auth.connectionError"); }
	finally { busy = false; }
}
</script>

<svelte:head><title>{$t("common.auth.signIn")} · Postie</title></svelte:head>
<div class="login-wrap">
	<div class="login-panel animate-fade-up">
		<img src={logo} alt="Postie" class="mx-auto h-14 w-14 object-contain" />
		<h1 class="mt-3 text-center text-lg font-semibold">{$t(registration ? "common.auth.createAdmin" : "common.auth.signIn")}</h1>
		{#if registration}<p class="mt-1 text-center text-sm text-base-content/65">{$t("common.auth.bootstrapDescription")}</p>{/if}
		{#if error}<p class="alert alert-error alert-soft mt-4 py-2 text-sm animate-fade-in" role="alert">{error}</p>{/if}
		{#if ready}
			<form onsubmit={submit} class="mt-5 flex flex-col gap-3.5" in:fade={{ speed: "base" }}>
				<label class="flex flex-col gap-1" for="username"><span class="text-sm font-medium">{$t("common.auth.username")}</span>
					<input id="username" name="username" class="input w-full" bind:value={username} autocomplete="username" required minlength={registration ? 3 : undefined} maxlength={registration ? 128 : 254} />
				</label>
				{#if registration}
					<label class="flex flex-col gap-1" for="email"><span class="text-sm font-medium">{$t("common.auth.email")}</span>
						<input id="email" name="email" type="email" class="input w-full" bind:value={email} autocomplete="email" maxlength="254" />
					</label>
				{/if}
				<label class="flex flex-col gap-1" for="password"><span class="text-sm font-medium">{$t("common.auth.password")}</span>
					<input id="password" name="password" type="password" class="input w-full" bind:value={password} autocomplete={registration ? "new-password" : "current-password"} required minlength={registration ? 12 : undefined} />
				</label>
				{#if registration}<p class="text-xs text-base-content/65">{$t("common.auth.passwordHint")}</p>{/if}
				<button class="btn btn-primary mt-1" type="submit" disabled={busy}>
					{#if busy}<span class="loading loading-spinner loading-xs"></span>{/if}
					{$t(registration ? "common.auth.createAdmin" : "common.auth.signIn")}
				</button>
			</form>
		{:else if !error}<div class="mt-5 flex justify-center"><span class="loading loading-spinner"></span></div>{/if}
	</div>
</div>

<style>
	.login-wrap {
		flex: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: 1.5rem 1rem;
		min-height: 0;
	}
	.login-panel {
		width: 100%;
		max-width: 22rem;
		padding: 1.75rem 1.5rem 1.5rem;
		border-radius: calc(var(--radius-box) * 1.25);
		background: var(--color-base-100);
		box-shadow: var(--ui-shadow-md);
	}
</style>
