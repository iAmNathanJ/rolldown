import {t} from './vendor.js';
export const c = () => t() + globalThis.C;
globalThis.lazy = () => import('./lazy.js');
