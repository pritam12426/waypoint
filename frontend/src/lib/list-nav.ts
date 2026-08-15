import { create } from "zustand";

export interface ListNavActions {
	onOpen?: (id: number) => void;
	onCopyUrl?: (id: number) => void;
	onToggleSelect?: (id: number) => void;
	onStar?: (id: number) => void;
	onArchive?: (id: number) => void;
	onEdit?: (id: number) => void;
	onTrash?: (id: number) => void;
}

interface ListNavState {
	/** ids in on-screen order, for j/k/gg/G movement */
	ids: number[];
	activeId: number | null;
	actions: ListNavActions;
	register: (ids: number[], actions: ListNavActions) => void;
	unregister: () => void;
	move: (direction: "up" | "down" | "first" | "last") => void;
	setActive: (id: number | null) => void;
}

export const useListNav = create<ListNavState>((set, get) => ({
	ids: [],
	activeId: null,
	actions: {},
	register: (ids, actions) =>
		set((state) => ({
			ids,
			actions,
			activeId:
				state.activeId !== null && ids.includes(state.activeId)
					? state.activeId
					: (ids[0] ?? null),
		})),
	unregister: () => set({ ids: [], activeId: null, actions: {} }),
	move: (direction) => {
		const { ids, activeId } = get();
		if (ids.length === 0) return;
		const currentIndex = activeId !== null ? ids.indexOf(activeId) : -1;
		let nextIndex: number;
		switch (direction) {
			case "up":
				nextIndex = Math.max(0, currentIndex - 1);
				break;
			case "down":
				nextIndex = Math.min(ids.length - 1, currentIndex + 1);
				break;
			case "first":
				nextIndex = 0;
				break;
			case "last":
				nextIndex = ids.length - 1;
				break;
		}
		set({ activeId: ids[nextIndex] });
	},
	setActive: (id) => set({ activeId: id }),
}));
