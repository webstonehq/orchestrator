import { writable } from 'svelte/store';

export type ToastVariant = 'info' | 'error';

export interface ToastItem {
	id: number;
	message: string;
	variant: ToastVariant;
}

const AUTO_DISMISS_MS = 4000;

let nextId = 1;

export const toasts = writable<ToastItem[]>([]);

export function dismiss(id: number): void {
	toasts.update((list) => list.filter((t) => t.id !== id));
}

function push(message: string, variant: ToastVariant): number {
	const id = nextId++;
	toasts.update((list) => [...list, { id, message, variant }]);
	setTimeout(() => dismiss(id), AUTO_DISMISS_MS);
	return id;
}

export const toast = {
	info: (message: string) => push(message, 'info'),
	error: (message: string) => push(message, 'error')
};
