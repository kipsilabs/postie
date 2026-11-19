// Bridge to the Tauri desktop shell. The page is the shell's own loopback server, so it runs in web
// mode over HTTP and only reaches native-only operations (pickers, save dialogs) through these commands.

type TauriInternals = {
	invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
};

function internals(): TauriInternals | undefined {
	if (typeof window === "undefined") return undefined;
	return (window as unknown as { __TAURI_INTERNALS__?: TauriInternals })
		.__TAURI_INTERNALS__;
}

export function isTauri(): boolean {
	return internals() !== undefined;
}

export async function tauriInvoke<T>(
	cmd: string,
	args?: Record<string, unknown>,
): Promise<T> {
	const i = internals();
	if (!i) throw new Error("Tauri is not available");
	return i.invoke<T>(cmd, args);
}
