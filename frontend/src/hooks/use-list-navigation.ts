import { useEffect } from "react";
import { type ListNavActions, useListNav } from "#/lib/list-nav";

/**
 * Registers the current list (its ids, in on-screen order, and its action
 * handlers) with the global vim-nav store. AppShell's single keydown
 * listener drives j/k/gg/G/o/Enter/Y/x/s/a/e/d against whatever list is
 * currently registered. Unregisters on unmount.
 */
export function useListNavigation(ids: number[], actions: ListNavActions) {
	const register = useListNav((s) => s.register);
	const unregister = useListNav((s) => s.unregister);

	// ids/actions are re-created every render by the caller; keying on the
	// stable identity of the id list (not the ids/actions objects) is
	// intentional here — register() and unregister() are stable Zustand refs.
	// biome-ignore lint/correctness/useExhaustiveDependencies: see above
	useEffect(() => {
		register(ids, actions);
		return () => unregister();
	}, [ids.join(",")]);

	const activeId = useListNav((s) => s.activeId);
	return { activeId };
}
