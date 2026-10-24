import {c} from './shared.js';
export const sk = () => c() + 1;
export function open() { return import('./editor.js'); }
export function slice() { return import('./slice.js'); }
globalThis.open = open;
globalThis.slice = slice;
