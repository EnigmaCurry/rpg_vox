import { writable } from 'svelte/store';

function currentHash() {
  const h = location.hash.replace(/^#/, '');
  return h || '/';
}

function createRoute() {
  const store = writable(currentHash());
  window.addEventListener('hashchange', () => store.set(currentHash()));
  return store;
}

export const route = createRoute();

export function navigate(path) {
  const normalized = path.startsWith('/') ? path : `/${path}`;
  location.hash = normalized;
}
